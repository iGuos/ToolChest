pub mod deepseek;
pub mod hosts;
pub mod http;
pub mod lan;
pub mod port;
pub mod trust_app;

/// 构造一个不弹控制台黑框的 Command（Windows 加 CREATE_NO_WINDOW；其他平台等同普通 Command）。
/// 用于 GUI 进程里调用 netstat/tasklist/powershell/reg 等控制台程序时，避免一闪而过的 cmd 窗口。
#[cfg_attr(not(windows), allow(dead_code, unused_mut))]
pub fn hidden_command(program: &str) -> std::process::Command {
    let mut c = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}
