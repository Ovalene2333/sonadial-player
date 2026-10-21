# Agent Instructions

## 测试与编译

- 禁止采用「改一点 → 跑 `cargo test` / 编译 → 再改一点」的反复循环。
- 禁止为纯逻辑新增或反复运行 Rust `assert` 测试。
- 解析、计时、单位换算、字符串处理等逻辑，优先用 Python 脚本快速验证。
- Python 验证脚本放在临时目录即可，不要提交到仓库。
- Rust 代码只在逻辑稳定后做一次 `cargo check`。
- 最终交付前再做一次 release 编译，不要每改一处都重新构建。
