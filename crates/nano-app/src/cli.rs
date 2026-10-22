//! Command-line surface for the Nano TUI.
//!
//! Every path is explicit; the app never guesses a session identity from a
//! directory name or title.

use std::path::PathBuf;

use nano_runtime::paths::{new_root_session_id, profile_dir};
use nano_runtime::{Host, HostOptions, SessionMode};

/// Parsed launch options.
#[derive(Debug, Clone, Default)]
pub struct Cli {
    /// Working directory for the official session.
    pub cwd: Option<PathBuf>,
    /// Nano data root.
    pub data_root: Option<PathBuf>,
    /// Official dsh home.
    pub dsh_home: Option<PathBuf>,
    /// Node executable override.
    pub node_bin: Option<PathBuf>,
    /// dsh launcher override.
    pub dsh_bin: Option<PathBuf>,
    /// Bridge package override.
    pub bridge_package: Option<PathBuf>,
    /// Root session to resume.
    pub resume: Option<String>,
    /// Initial provider route for the root Agent.
    pub provider: Option<String>,
    /// Initial model for the root Agent.
    pub model: Option<String>,
    /// Initial reasoning effort for the root Agent.
    pub effort: Option<String>,
    /// Force a profile install before launch.
    pub install: bool,
    /// Skip the profile-presence check.
    pub no_install: bool,
    /// Do not use the alternate screen (keeps scrollback for evidence).
    pub no_alt_screen: bool,
    /// Print help and exit.
    pub help: bool,
}

impl Cli {
    /// Parse process arguments.
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Self, String> {
        let mut cli = Self::default();
        let mut args = args.into_iter();
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--help" | "-h" => cli.help = true,
                "--install" => cli.install = true,
                "--no-install" => cli.no_install = true,
                "--no-alt-screen" => cli.no_alt_screen = true,
                "--cwd" => cli.cwd = Some(PathBuf::from(value(&mut args, "--cwd")?)),
                "--data-root" => {
                    cli.data_root = Some(PathBuf::from(value(&mut args, "--data-root")?))
                }
                "--dsh-home" => cli.dsh_home = Some(PathBuf::from(value(&mut args, "--dsh-home")?)),
                "--node" => cli.node_bin = Some(PathBuf::from(value(&mut args, "--node")?)),
                "--dsh-bin" => cli.dsh_bin = Some(PathBuf::from(value(&mut args, "--dsh-bin")?)),
                "--bridge-package" => {
                    cli.bridge_package = Some(PathBuf::from(value(&mut args, "--bridge-package")?))
                }
                "--resume" => cli.resume = Some(value(&mut args, "--resume")?),
                "--provider" => cli.provider = Some(value(&mut args, "--provider")?),
                "--model" => cli.model = Some(value(&mut args, "--model")?),
                "--effort" => cli.effort = Some(value(&mut args, "--effort")?),
                other => return Err(format!("unknown argument {other}; use --help")),
            }
        }
        Ok(cli)
    }

    /// Build host options from the CLI.
    pub fn host_options(&self) -> HostOptions {
        HostOptions {
            cwd: self.cwd.clone(),
            data_root: self.data_root.clone(),
            dsh_home: self.dsh_home.clone(),
            node_bin: self.node_bin.clone(),
            dsh_bin: self.dsh_bin.clone(),
            bridge_package: self.bridge_package.clone(),
            handshake_timeout: Some(std::time::Duration::from_secs(60)),
            request_timeout: Some(std::time::Duration::from_secs(30)),
            test_faults: None,
            force_fallback: false,
            provider: self.provider.clone(),
            model: self.model.clone(),
            reasoning_effort: self.effort.clone(),
            terminal_unattended_timeout_ms: None,
            terminal_activity_poll_interval_ms: None,
        }
    }

    /// Resolve the root session identity and mode. A resumed identity must be
    /// a safe opaque id before it reaches any host-owned path join.
    pub fn session(&self) -> Result<(String, SessionMode), String> {
        match &self.resume {
            Some(id) if !id.is_empty() => {
                let safe = id.len() <= 128
                    && id.chars().all(|character| {
                        character.is_ascii_alphanumeric() || character == '_' || character == '-'
                    });
                if !safe {
                    return Err(format!(
                        "--resume id {id:?} is not a safe session identity; expected [A-Za-z0-9_-]{{1,128}}"
                    ));
                }
                Ok((id.clone(), SessionMode::Resume))
            }
            _ => Ok((new_root_session_id(), SessionMode::Create)),
        }
    }

    /// Install the profile when it is missing or explicitly requested.
    ///
    /// The check reads the profile manifest rather than booting dsh, so a normal
    /// launch stays fast. A profile owned by another product is never touched;
    /// the installer reports that error.
    pub fn ensure_profile(&self, options: &HostOptions) -> Result<(), String> {
        if self.no_install && !self.install {
            return Ok(());
        }
        let preflight =
            Host::preflight(options).map_err(|error| format!("preflight failed: {error}"))?;
        if !self.install && profile_ready(&preflight.paths) {
            return Ok(());
        }
        eprintln!("nano: installing the dsh-tui-nano profile (first launch or --install)");
        let summary = Host::install_profile(options)
            .map_err(|error| format!("profile install failed: {error}"))?;
        eprintln!(
            "nano: profile ready at {} (installed: {})",
            summary.profile_dir.display(),
            summary.installed
        );
        Ok(())
    }
}

fn profile_ready(paths: &nano_runtime::RuntimePaths) -> bool {
    let manifest = profile_dir(paths).join("package.json");
    let Ok(text) = std::fs::read_to_string(manifest) else {
        return false;
    };
    text.contains("@dsh-tui-nano/bridge")
}

fn value<I: Iterator<Item = String>>(args: &mut I, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} needs a value"))
}

/// Help text.
pub const HELP: &str = "\
nano — 极简 DeepSeek Harness 终端界面

用法：
  nano [选项]

选项：
  --cwd <path>             官方会话的工作目录（默认当前目录）
  --data-root <path>       Nano 数据根（默认 ~/.dsh-tui-nano）
  --dsh-home <path>        官方 dsh home（默认 $DSH_HOME 或 ~/.dsh）
  --resume <session-id>    恢复指定根会话；缺省创建新会话
  --provider <id>          初始模型 provider（需与 --model 同时给出）
  --model <id>             初始模型
  --effort <id>            初始推理强度
  --install                强制重新安装 Nano profile
  --no-install             跳过 profile 检查（假定已安装）
  --no-alt-screen          不使用备用屏幕，保留终端滚动记录
  --node <path>            node 可执行文件
  --dsh-bin <path>         dsh 启动器
  --bridge-package <path>  桥接包目录
  -h, --help               显示本帮助

环境变量：
  NANO_THEME=dark|light    diff 底色主题（默认 dark，只影响差异底色）
  NANO_REDUCED_MOTION=1    关闭忙碌指示与子代理状态灯动效

按键：
  Enter 发送/排队；Alt+Enter 插话；Ctrl+J 换行（Shift+Enter 需终端支持键盘增强）
  Esc 停止当前回合（保留队列）；Ctrl+Y 复制草稿原文（OSC 52）；Ctrl+C / Ctrl+Q 退出
  Ctrl+P 命令面板；/model 模型与推理强度；/permission 权限预设（bare 打开面板，带参数走官方命令）
  Alt+O 会话列表；Alt+C 压缩；Ctrl+G 调度面板；Alt+L 全部子代理
  @ 引用文件，Tab 接受补全候选；Alt+A 附加图片/文件；Alt+←/→ 排序附件；Alt+Backspace 移除附件
  Ctrl+U 清空草稿与附件
  Alt+↑/↓ 选择队列项；Alt+E 编辑；Alt+D 删除；Alt+S 转为插话；Alt+X 清空并停止
  !命令 人工命令：以你的用户权限运行，不是 Agent 授权；输出作为合成上下文注入，不唤醒模型
  Ctrl+T 任务面板：↑↓ 选择，Enter 查看输出（非消费跟随），k 停止，r 刷新
  Alt+K 技能目录（↑↓ 选择、Enter 填入 /name、P 预览）；Alt+R 项目规矩来源
  子代理 dock：↑↓ 选择、Enter/→/Space 进入、f 打开记录、k 停止、l 全部、Esc 返回
  子代理详情：Esc 或空输入 ← 返回、Alt+K 停止本轮
  PageUp/PageDown 翻阅记录；Ctrl+End 回到底部
  F12 进入/返回交互终端；终端内 F11 关闭、F10 重新附着、F9 选择 shell
  终端内 F8 复制所选文本（OSC 52；鼠标松开也会自动复制）
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_lists_every_composer_key() {
        for key in [
            "@ 引用文件",
            "Alt+A",
            "Alt+←/→",
            "Alt+Backspace",
            "Ctrl+U",
            "Tab 接受补全候选",
            "Ctrl+Q",
            "Alt+O",
            "Alt+C",
            "Ctrl+G",
            "Alt+L",
            "子代理 dock",
            "子代理详情",
        ] {
            assert!(HELP.contains(key), "help is missing {key}");
        }
    }

    #[test]
    fn unsafe_resume_ids_are_rejected_before_the_host_starts() {
        let mut cli = Cli::default();
        for id in ["../escape", "/absolute", "with/slash", "with space"] {
            cli.resume = Some(id.to_string());
            assert!(cli.session().is_err(), "{id} must not be resumed");
        }
        cli.resume = Some("session-safe_123".to_string());
        assert!(cli.session().is_ok());
        cli.resume = None;
        let (generated, mode) = cli.session().expect("generated id");
        assert!(matches!(mode, SessionMode::Create));
        assert!(generated.starts_with("session-"));
    }
}
