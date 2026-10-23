// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! The parent–child lifetime contract of the runtime tree.
//!
//! Ownership runs one way: the parent holds its children **weakly**, the child
//! holds its parent **strongly**. Every rule below follows from that pair, and
//! each one is pinned by a test in this file:
//!
//! 1. **A parent outlives its children.** A child keeps its parent alive, so
//!    the parent's heap, contexts and jobs stay usable after the host lets go
//!    of the parent's handle.
//! 2. **A child lives exactly as long as the host's handle.** Dropping the
//!    handle frees the child for good; the parent stops driving it and prunes
//!    it from its tick list.
//! 3. **The tree is released as one piece.** Letting go of the last handle of
//!    a subtree frees the whole subtree — in either release order.
//! 4. **A child can re-enter its parent**, including while the parent is
//!    ticking it. That is what the tree tick's borrow structure exists for:
//!    the parent must not hold a live `&mut RuntimeImpl` across a child's
//!    tick, or the re-entry would alias it.
//! 5. **Only a root's interrupt stops the whole tree.** A child's interrupt
//!    stops that child alone.
//!
//! These are engine-level guarantees: no host code takes part in them.

#[cfg(all(test, feature = "child_runtime"))]
mod tests {
    use std::{
        cell::{Cell, RefCell},
        process::ExitCode,
        rc::Rc,
    };

    use crate::{
        Context, JSValue, Runtime, ScopedContext,
        eval::{EvalFlags, EvalName, EvalType},
        runtime::{TickStatus, handle::RUNTIMES},
    };

    /// Queue a one-shot job in `ctx` that flips `flag` when it runs.
    fn arm(ctx: &mut Context<'static>, flag: Rc<Cell<bool>>) {
        ctx.queue_oneshot_job(
            move |_cx| {
                flag.set(true);
                Ok(())
            },
            (),
        );
    }

    /// Drive every root runtime registered on this thread, the way the
    /// scheduler does. Needed when the host no longer holds a root handle.
    fn tick_roots() {
        RUNTIMES.with(|rts| {
            let rts = rts.borrow();
            for w in rts.iter() {
                let Some(rc) = w.upgrade() else { continue };
                let mut rt = Runtime::from_inner(rc);
                rt.tick();
            }
        });
    }

    /// A weak view of a runtime, to watch it die.
    fn weak(rt: &Runtime) -> std::rc::Weak<std::cell::UnsafeCell<crate::RuntimeImpl>> {
        Rc::downgrade(&rt.0)
    }

    /// The number of entries the parent keeps for its children.
    fn child_count(rt: &Runtime) -> usize {
        unsafe { (&*rt.0.get()).children.len() }
    }

    // ── rule 1: a parent outlives its children ────────────────────────────

    /// The child keeps the parent alive, so the parent is not merely
    /// reachable but **intact**: its context still evaluates and its jobs
    /// still run after the host lets go of its handle.
    #[test]
    fn a_parent_outlives_its_children() -> Result<(), i32> {
        let mut root = Runtime::create()?;
        let mut root_ctx: Context<'static> =
            root.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;
        let mut child = Runtime::create_child(&root)?;

        // The host lets go of the parent — the child still holds it.
        drop(root);
        assert!(
            child.parent().is_some(),
            "the child must keep the parent alive"
        );

        // The parent's own context still works.
        let v = root_ctx.eval(
            b"6 * 7",
            &EvalName::Name("root.js".into()),
            EvalType::Global,
            EvalFlags::empty(),
        )?;
        assert_eq!(v.as_int(), Some(42));

        // ...and the child is still driven: the root is still reachable, so
        // the scheduler keeps ticking the tree.
        let ran = Rc::new(Cell::new(false));
        let mut child_ctx: Context<'static> =
            child.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;
        arm(&mut child_ctx, ran.clone());
        tick_roots();
        assert!(ran.get(), "the child must still be driven");
        Ok(())
    }

    // ── rule 2: a child lives as long as the host's handle ───────────────

    /// Dropping the child's handle frees it for good: the parent's tick list
    /// holds it weakly, so the entry no longer upgrades — and the parent
    /// prunes it instead of growing the list forever.
    #[test]
    fn a_child_lives_exactly_as_long_as_its_handle() -> Result<(), i32> {
        let mut root = Runtime::create()?;
        let mut child = Runtime::create_child(&root)?;
        let ran = Rc::new(Cell::new(false));
        {
            let mut cctx: Context<'static> =
                child.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;
            arm(&mut cctx, ran.clone());
        }

        let child_weak = weak(&child);
        assert!(child_weak.upgrade().is_some(), "the child starts alive");

        // The host lets go of the child.
        drop(child);
        assert!(
            child_weak.upgrade().is_none(),
            "a released child must be freed, not merely detached"
        );

        root.tick();
        assert!(!ran.get(), "a dead child must not be driven");
        assert_eq!(child_count(&root), 0, "the dead child must be pruned");
        Ok(())
    }

    /// Releasing one child leaves its siblings alone: they are still driven,
    /// and the tree keeps working.
    #[test]
    fn a_released_child_does_not_disturb_its_siblings() -> Result<(), i32> {
        let mut root = Runtime::create()?;
        let a = Runtime::create_child(&root)?;
        let mut b = Runtime::create_child(&root)?;

        let b_ran = Rc::new(Cell::new(false));
        {
            let mut cctx: Context<'static> = b.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;
            arm(&mut cctx, b_ran.clone());
        }

        drop(a);
        root.tick();
        assert!(b_ran.get(), "the surviving sibling must still be driven");
        assert_eq!(child_count(&root), 1, "only the dead entry is pruned");
        Ok(())
    }

    /// A grandchild that is released leaves the middle runtime alone.
    #[test]
    fn a_released_grandchild_does_not_disturb_the_middle() -> Result<(), i32> {
        let mut root = Runtime::create()?;
        let mut child = Runtime::create_child(&root)?;
        let grandchild = Runtime::create_child(&child)?;

        let child_ran = Rc::new(Cell::new(false));
        {
            let mut cctx: Context<'static> =
                child.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;
            arm(&mut cctx, child_ran.clone());
        }

        drop(grandchild);
        root.tick();
        assert!(child_ran.get(), "the middle runtime must still be driven");
        Ok(())
    }

    // ── rule 3: the tree is released as one piece ─────────────────────────

    /// Letting go of the parent's handle first, then the child's, frees the
    /// whole tree: the child kept the parent alive until it was itself gone.
    #[test]
    fn releasing_parent_then_child_frees_the_tree() -> Result<(), i32> {
        let root = Runtime::create()?;
        let child = Runtime::create_child(&root)?;

        let root_weak = weak(&root);
        let child_weak = weak(&child);

        drop(root);
        assert!(
            root_weak.upgrade().is_some(),
            "the child must keep the parent alive"
        );

        drop(child);
        assert!(child_weak.upgrade().is_none(), "the child must be freed");
        assert!(
            root_weak.upgrade().is_none(),
            "the parent must be freed once the child is gone"
        );
        Ok(())
    }

    /// The other release order frees the tree just as completely.
    #[test]
    fn releasing_child_then_parent_frees_the_tree() -> Result<(), i32> {
        let root = Runtime::create()?;
        let child = Runtime::create_child(&root)?;

        let root_weak = weak(&root);
        let child_weak = weak(&child);

        drop(child);
        assert!(child_weak.upgrade().is_none(), "the child must be freed");
        assert!(
            root_weak.upgrade().is_some(),
            "the parent is still held by the host"
        );

        drop(root);
        assert!(
            root_weak.upgrade().is_none(),
            "the parent must be freed once the host lets go"
        );
        Ok(())
    }

    /// A subtree is kept alive by its own leaves, not by the handle that
    /// rooted it: releasing the middle runtime's handle leaves it alive (the
    /// grandchild holds it) and still driven; releasing the grandchild too
    /// takes the middle with it.
    #[test]
    fn a_subtree_survives_its_root_handle_but_not_its_leaf_handle() -> Result<(), i32> {
        let mut root = Runtime::create()?;
        let child = Runtime::create_child(&root)?;
        let mut grandchild = Runtime::create_child(&child)?;

        let grandchild_ran = Rc::new(Cell::new(false));
        {
            let mut gctx: Context<'static> =
                grandchild.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;
            arm(&mut gctx, grandchild_ran.clone());
        }

        // The host lets go of the middle runtime only.
        let middle_weak = weak(&child);
        drop(child);
        root.tick();
        assert!(
            grandchild_ran.get(),
            "the grandchild keeps the middle alive, so it must be driven"
        );

        // Now the grandchild goes too — and the middle follows it, because
        // the grandchild was the only thing holding the middle.
        drop(grandchild);
        assert!(
            middle_weak.upgrade().is_none(),
            "the middle must go once its last holder is gone"
        );
        Ok(())
    }

    // ── rule 4: a child can re-enter its parent ───────────────────────────

    /// A child can call into its parent's heap — even after the host let go
    /// of the parent's handle, because the child is what keeps the parent
    /// alive.
    #[test]
    fn a_child_can_reenter_its_parent_after_the_parent_handle_is_dropped() -> Result<(), i32> {
        let mut root = Runtime::create()?;
        let mut root_ctx: Context<'static> =
            root.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;
        let f = root_ctx.with_root_scope(|mut cx| {
            cx.eval(
                b"(x) => x * 2",
                &EvalName::Name("root.js".into()),
                EvalType::Global,
                EvalFlags::empty(),
            )
        })?;

        let mut child = Runtime::create_child(&root)?;
        let mut child_ctx: Context<'static> =
            child.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;

        // The host lets go of the parent.
        drop(root);

        // The child still reaches the parent's heap.
        let out = child_ctx
            .with_root_scope(|_cx| Context::call_in_ctx(&root_ctx, f, &[JSValue::from(21)]))?;
        assert_eq!(out.as_int(), Some(42));
        Ok(())
    }

    /// A child can re-enter its parent **while the parent is ticking it**.
    ///
    /// This is the case the tree tick's borrow structure exists for: the
    /// parent must not hold a live `&mut RuntimeImpl` across the child's
    /// tick, or the re-entry would alias it.
    #[test]
    fn a_child_can_reenter_its_parent_while_being_ticked() -> Result<(), i32> {
        let mut root = Runtime::create()?;
        let mut root_ctx: Context<'static> =
            root.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;
        root_ctx.with_root_scope(|mut cx| {
            cx.eval(
                b"globalThis.__base = 40;",
                &EvalName::Name("root.js".into()),
                EvalType::Global,
                EvalFlags::empty(),
            )?;
            Ok::<_, i32>(())
        })?;
        let f = root_ctx.with_root_scope(|mut cx| {
            cx.eval(
                b"(x) => globalThis.__base + x",
                &EvalName::Name("root.js".into()),
                EvalType::Global,
                EvalFlags::empty(),
            )
        })?;

        let mut child = Runtime::create_child(&root)?;
        let mut child_ctx: Context<'static> =
            child.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;

        // The child's job calls into the parent while the root drives it.
        let parent_ctx = root_ctx;
        let result: Rc<RefCell<Option<i32>>> = Rc::new(RefCell::new(None));
        let result_2 = result.clone();
        child_ctx.queue_oneshot_job(
            move |_cx| {
                let out = Context::call_in_ctx(&parent_ctx, f, &[JSValue::from(2)]);
                *result_2.borrow_mut() = out.ok().and_then(|v| v.as_int());
                Ok(())
            },
            (),
        );

        root.tick();
        assert_eq!(
            *result.borrow(),
            Some(42),
            "the child must reach the parent's heap"
        );
        Ok(())
    }

    // ── rule 5: only a root's interrupt stops the tree ────────────────────

    /// A child's interrupt stops that child alone; a root's interrupt stops
    /// the whole tree.
    #[test]
    fn only_a_root_interrupt_stops_the_tree() -> Result<(), i32> {
        let mut root = Runtime::create()?;
        let mut a = Runtime::create_child(&root)?;
        let mut b = Runtime::create_child(&root)?;

        let b_ran = Rc::new(Cell::new(false));
        {
            let mut cctx: Context<'static> = b.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;
            arm(&mut cctx, b_ran.clone());
        }

        // A child's interrupt leaves the rest of the tree running.
        a.interrupt(ExitCode::from(1));
        root.tick();
        assert!(b_ran.get(), "a child's interrupt must not stop the tree");

        // A root interrupt stops everything.
        let c_ran = Rc::new(Cell::new(false));
        let mut c = Runtime::create_child(&root)?;
        {
            let mut cctx: Context<'static> = c.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;
            arm(&mut cctx, c_ran.clone());
        }
        root.interrupt(ExitCode::from(1));
        assert_eq!(root.tick(), TickStatus::Idle);
        assert!(!c_ran.get(), "a root interrupt must stop the whole tree");
        Ok(())
    }

    // ── the tick list ─────────────────────────────────────────────────────

    /// A child created while a *child* is ticking must not be lost: the
    /// parent's tick list is read, not emptied, so the new child lands in it
    /// and is driven from the next round on.
    #[test]
    fn a_child_created_during_a_child_tick_is_kept() -> Result<(), i32> {
        let mut root = Runtime::create()?;
        let mut child = Runtime::create_child(&root)?;
        let mut child_ctx: Context<'static> =
            child.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;

        // The child's job creates a sibling of itself — while the root is
        // driving the child, i.e. while the root walks its tick list.
        let root_weak = weak(&root);
        let sibling: Rc<RefCell<Option<Runtime>>> = Rc::new(RefCell::new(None));
        let sibling_2 = sibling.clone();
        child_ctx.queue_oneshot_job(
            move |_cx| {
                let Some(rc) = root_weak.upgrade() else {
                    return Ok(());
                };
                let parent = Runtime::from_inner(rc);
                *sibling_2.borrow_mut() = Some(Runtime::create_child(&parent)?);
                Ok(())
            },
            (),
        );

        root.tick();

        let sibling_rt = sibling
            .borrow()
            .clone()
            .expect("the sibling must have been created");
        let listed = unsafe { (&*root.0.get()).children.iter() }.any(|w| {
            w.upgrade()
                .is_some_and(|rc| Runtime::ptr_eq(&Runtime::from_inner(rc), &sibling_rt))
        });
        assert!(listed, "the sibling must stay in the parent's tick list");

        // ...and the next round drives it.
        let ran = Rc::new(Cell::new(false));
        let mut sibling_rt = sibling_rt.clone();
        let mut sibling_ctx: Context<'static> =
            sibling_rt.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;
        arm(&mut sibling_ctx, ran.clone());
        root.tick();
        assert!(ran.get(), "the sibling must be driven by the next round");
        Ok(())
    }

    /// The tick list does not grow without bound as children come and go.
    #[test]
    fn the_tick_list_does_not_grow_with_released_children() -> Result<(), i32> {
        let mut root = Runtime::create()?;
        for _ in 0..10 {
            let mut c = Runtime::create_child(&root)?;
            let _ = c.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;
        }
        assert_eq!(child_count(&root), 10, "every child is listed");
        root.tick();
        assert_eq!(child_count(&root), 0, "the dead entries are pruned");
        Ok(())
    }

    /// Detaching a child is idempotent and ignores strangers: it only
    /// removes the entry from the tick list, and never decides the child's
    /// lifetime.
    #[test]
    fn remove_child_is_idempotent_and_ignores_strangers() -> Result<(), i32> {
        let root = Runtime::create()?;
        let stranger = Runtime::create()?;
        let child = Runtime::create_child(&root)?;

        root.remove_child(&child);
        assert_eq!(child_count(&root), 0, "the child is detached");
        root.remove_child(&child);
        assert_eq!(child_count(&root), 0, "detaching twice is a no-op");
        root.remove_child(&stranger);
        assert_eq!(child_count(&root), 0, "a stranger is not our child");

        // Detaching does not kill the child — the host's handle still does.
        assert!(weak(&child).upgrade().is_some());
        drop(child);
        Ok(())
    }

    // ── the role marker ───────────────────────────────────────────────────

    /// The role marker is the parent link itself: a child has a parent, a
    /// root does not.
    #[test]
    fn the_parent_link_is_the_role_marker() -> Result<(), i32> {
        let root = Runtime::create()?;
        let child = Runtime::create_child(&root)?;

        assert!(root.is_root() && !root.is_child());
        assert!(child.is_child() && !child.is_root());
        assert!(child.parent().is_some());
        assert!(root.parent().is_none());
        Ok(())
    }

    /// The parent chain is a **tree**: the child is created fresh at every
    /// level, so no runtime is ever its own parent and the chain has exactly
    /// one node per level of nesting.
    ///
    /// `create_child_with` is the only place the link is established, and it
    /// always builds a new child — a runtime cannot be handed in as its own
    /// parent, and the chain can never close into a cycle.
    #[test]
    fn the_parent_chain_is_a_tree() -> Result<(), i32> {
        let root = Runtime::create()?;
        let child = Runtime::create_child(&root)?;
        let grandchild = Runtime::create_child(&child)?;

        // Walk up from the deepest node.
        let mut depth = 0usize;
        let mut node = grandchild.clone();
        while let Some(parent) = node.parent() {
            assert!(
                !Runtime::ptr_eq(&parent, &node),
                "a runtime cannot be its own parent"
            );
            depth += 1;
            node = parent;
        }
        assert_eq!(depth, 2, "the chain is as long as the nesting");
        assert!(
            Runtime::ptr_eq(&node, &root),
            "the chain must end at the root"
        );
        Ok(())
    }
}
