// Placeholder: the test262 runner loads `<base_path>/harness/assert.js`
// unconditionally before every script. The IR-blowup repro calls no harness
// helper, so an empty file is enough to keep the repro self-contained (no
// dependency on an external test262 checkout).