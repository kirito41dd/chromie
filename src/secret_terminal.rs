//! 在显示 Secret 提示前关闭回显，覆盖两次输入及 rpassword 恢复设置的间隙。
use crate::error::{Error, Result};
use std::future::Future;

#[derive(Default)]
pub(crate) struct Cancellation {
    #[cfg(unix)]
    signal: Option<tokio::signal::unix::Signal>,
}
impl Cancellation {
    pub(crate) async fn run<T>(mut self, future: impl Future<Output = Result<T>>) -> Result<T> {
        #[cfg(unix)]
        if let Some(signal) = self.signal.as_mut() {
            return tokio::select! {
                biased;
                _ = signal.recv() => Err(Error::invalid("已停止等待；写入结果可能未知，重试须复用本次 request_id")),
                result = future => result,
            };
        }
        future.await
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use rustix::termios::{LocalModes, OptionalActions, Termios, tcgetattr, tcsetattr};
    use std::fs::{File, OpenOptions};
    use tokio::signal::unix::{SignalKind, signal};

    struct EchoGuard {
        tty: File,
        original: Termios,
        restored: bool,
    }
    impl EchoGuard {
        fn start() -> Result<Self> {
            let tty = OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")
                .map_err(|_| Error::invalid("无法打开本机交互终端"))?;
            let original =
                tcgetattr(&tty).map_err(|_| Error::invalid("无法读取终端设置，未提示输入"))?;
            let mut hidden = original.clone();
            // rpassword 将 Ctrl-C 作为字符读取后发 SIGINT。先禁用 ISIG，
            // 让提示刚出现时按下的 Ctrl-C 也走可恢复设置的错误返回路径。
            hidden
                .local_modes
                .remove(LocalModes::ECHO | LocalModes::ECHONL | LocalModes::ISIG);
            let guard = Self {
                tty,
                original,
                restored: false,
            };
            tcsetattr(&guard.tty, OptionalActions::Now, &hidden)
                .map_err(|_| Error::invalid("无法关闭终端回显，未提示输入"))?;
            Ok(guard)
        }
        fn restore(&mut self) -> Result<()> {
            tcsetattr(&self.tty, OptionalActions::Now, &self.original)
                .map_err(|_| Error::invalid("终端设置恢复失败，请在终端运行 stty sane"))?;
            self.restored = true;
            Ok(())
        }
    }
    impl Drop for EchoGuard {
        fn drop(&mut self) {
            if !self.restored {
                let _ = self.restore();
            }
        }
    }

    pub(crate) struct Session {
        terminal: EchoGuard,
        cancellation: Cancellation,
    }
    impl Session {
        pub(crate) fn start() -> Result<Self> {
            // 先注册处理器，避免 rpassword 发 SIGINT 时直接终止进程而跳过 Drop。
            // 文件模式不注册；输入后将监听器移交给 HTTP 等待，Ctrl-C 仍可取消。
            let signal = signal(SignalKind::interrupt())
                .map_err(|_| Error::invalid("无法准备终端取消处理，未提示输入"))?;
            Ok(Self {
                terminal: EchoGuard::start()?,
                cancellation: Cancellation {
                    signal: Some(signal),
                },
            })
        }
        pub(crate) fn finish(mut self) -> Result<Cancellation> {
            self.terminal.restore()?;
            Ok(self.cancellation)
        }
    }
}
#[cfg(unix)]
pub(crate) use unix::Session;

#[cfg(not(unix))]
pub(crate) struct Session;
#[cfg(not(unix))]
impl Session {
    pub(crate) fn start() -> Result<Self> {
        Err(Error::invalid("首版 Secret 隐藏输入支持 macOS 与 Linux"))
    }
    pub(crate) fn finish(self) -> Result<Cancellation> {
        Ok(Cancellation::default())
    }
}
