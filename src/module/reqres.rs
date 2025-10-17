use crate::{
    atom::{Atom, AtomRef},
    branch_hints::unlikely,
    context::JSContext,
    module::ModuleDef,
};

#[derive(Debug)]
pub struct ModuleDependency {
    pub module_name: Atom,
    /// None if module is not resolved.
    pub module: Option<ModuleDef>,
}

impl ModuleDef {
    /// add module dependency, returns the dependency index.
    /// `module_name` is not consumed.
    pub fn add_dependency(&self, ctx: &JSContext, module_name: AtomRef) -> Result<u16, i32> {
        let mut lst = self.0.dependencies.borrow_mut();

        if let Some(i) = lst
            .iter()
            .position(|req| req.module_name.as_ref() == module_name)
        {
            // dep is already present, returns dep index directly
            return Ok(i as u16);
        }

        if unlikely(lst.len() == lst.capacity()) {
            if unlikely(lst.try_reserve(1).is_err()) {
                ctx.throw_out_of_memory();
                return Err(-1);
            }
        }

        lst.push(ModuleDependency {
            module_name: module_name.dup(&ctx.rt()),
            module: None,
        });

        // return the new dep index
        Ok(lst.len() as u16 - 1)
    }
}

impl JSContext {
    /// find the loaded module by module name.
    /// Note: `name` is not consumed.
    pub fn find_loaded_module(&self, name: AtomRef) -> Option<ModuleDef> {
        self.0.borrow_mut().loaded_modules.iter().find_map(|m| {
            if m.0.module_name.as_ref() == name {
                Some(m.clone())
            } else {
                None
            }
        })
    }

    /// Load all the required modules (dependent modules) for module 'm'.
    /// TODO: migrate to ModuleDef::resolve()
    pub fn resolve_module(&self, m: &ModuleDef) -> Result<(), i32> {
        if m.0.resolved.get() {
            return Ok(());
        }

        m.0.resolved.set(true);
        let base_module_name = m.0.module_name.as_ref();

        // resolve dependency modules
        for dep in m.0.dependencies.borrow_mut().iter_mut() {
            let m1 = js_host_resolve_imported_module_atom(
                self,
                base_module_name,
                dep.module_name.as_ref(),
            )?;
            dep.module = Some(m1.clone());
            self.resolve_module(&m1)?; // <- ensure not mut borrow .dependencies in calling graph
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JSResolveResultEnum {
    JS_RESOLVE_RES_EXCEPTION = -1, /* memory alloc error */
    JS_RESOLVE_RES_FOUND = 0,
    JS_RESOLVE_RES_NOT_FOUND,
    JS_RESOLVE_RES_CIRCULAR,
    JS_RESOLVE_RES_AMBIGUOUS,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum JSModuleStatus {
    JS_MODULE_STATUS_UNLINKED,
    JS_MODULE_STATUS_LINKING,
    JS_MODULE_STATUS_LINKED,
    JS_MODULE_STATUS_EVALUATING,
    JS_MODULE_STATUS_EVALUATING_ASYNC,
    JS_MODULE_STATUS_EVALUATED,
}

pub fn js_host_resolve_imported_module(
    ctx: &JSContext,
    base_cname: &str,
    cname1: &str,
) -> Result<ModuleDef, i32> {
    let name = if let Some(cb) = ctx.rt().module_normalize_func {
        cb(ctx, base_cname, cname1, ctx.rt().module_loader_opaque)
    } else {
        js_default_module_normalize_name(ctx, base_cname, cname1)
    }?;

    let module_name = Atom::new(ctx.rt_mut(), &name)?.into_rc(ctx.rt());

    // search in loaded_modules
    if let Some(m) = ctx.find_loaded_module(module_name.as_ref()) {
        return Ok(m);
    }

    // load the module
    if let Some(cb) = ctx.rt().module_loader_func {
        cb(ctx, &name, ctx.rt().module_loader_opaque)
    } else {
        #[cfg(debug_assertions)]
        println!("WARN: module loader function is not set, can't import module.");

        ctx.throw_reference_error(&format!("could not load module '{}'", name));
        Err(-1)
    }
}

pub fn js_host_resolve_imported_module_atom(
    ctx: &JSContext,
    base_module_name: AtomRef,
    module_name1: AtomRef,
) -> Result<ModuleDef, i32> {
    let base_cname = base_module_name.to_utf8(ctx.rt());
    let cname = module_name1.to_utf8(ctx.rt());
    js_host_resolve_imported_module(ctx, base_cname.as_ref(), &cname)
}

/// default module filename normalizer
fn js_default_module_normalize_name(
    _: &JSContext,
    base_name: &str,
    name: &str,
) -> Result<String, i32> {
    debug_assert!(!name.is_empty());

    // 如果不是相对路径，直接返回原名
    if !name.starts_with('.') {
        return Ok(name.to_string());
    }

    // 获取基础路径的目录部分
    let base_dir = if let Some(pos) = base_name.rfind('/') {
        &base_name[..pos]
    } else {
        ""
    };

    // 构建初始文件路径
    let mut filename = if !base_dir.is_empty() {
        format!("{}/", base_dir)
    } else {
        String::new()
    };

    // 标准化路径处理
    let mut r = name;
    loop {
        if r.starts_with("./") {
            r = &r[2..];
        } else if r.starts_with("../") {
            // 移除filename的最后一级路径元素，但不能是"."或".."
            if !filename.is_empty() {
                // 查找最后一个'/'的位置
                let mut parts: Vec<&str> = filename.split('/').collect();
                if !parts.is_empty() {
                    let last_part = parts.last().unwrap_or(&"");
                    // 如果最后一部分是"."或".."则停止处理
                    if *last_part != "." && *last_part != ".." {
                        // 移除最后一部分
                        if parts.len() > 1 {
                            parts.pop(); // 移除最后一部分
                            filename = parts.join("/");
                            if !filename.is_empty() {
                                filename.push('/');
                            }
                        } else {
                            filename.clear();
                        }
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
            r = &r[3..];
        } else {
            break;
        }
    }

    // 添加剩余的路径部分
    if !filename.is_empty() && !r.is_empty() {
        filename.push_str(r);
    } else if !r.is_empty() {
        filename.push_str(r);
    }

    Ok(filename)

    // let path = if let Some(base) = PathBuf::from(base_name).parent() {
    //     base.join(name)
    // } else {
    //     PathBuf::from(name)
    // };

    // let mut normalized = PathBuf::new();
    // for component in path.components() {
    //     match component {
    //         std::path::Component::ParentDir => {
    //             normalized.pop();
    //         }
    //         std::path::Component::CurDir => {}
    //         _ => {
    //             normalized.push(component.as_os_str());
    //         }
    //     }
    // }

    // Ok(normalized.to_string_lossy().to_string())
}
