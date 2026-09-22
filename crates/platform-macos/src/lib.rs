//! macOS 平台集成：路径、钥匙串、系统 PAC、编码代理配置、CLI 安装、运行时文件。

pub mod agents;
pub mod cli_install;
pub mod keychain;
pub mod network;
pub mod paths;
pub mod runtime;
pub mod shell;
