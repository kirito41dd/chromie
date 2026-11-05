//! 两个命令入口共用的中文帮助与安全错误输出，不回显用户参数值。
use clap::{Command, CommandFactory, Parser, error::ErrorKind};

fn localize(mut command: Command) -> Command {
    command = command
        .help_template("{about-with-newline}\n用法：{usage}\n\n{all-args}{after-help}")
        .subcommand_help_heading("命令")
        .subcommand_value_name("命令")
        .max_term_width(100)
        .mut_args(|arg| {
            let heading = if arg.is_positional() {
                "参数"
            } else {
                "选项"
            };
            let choices = arg
                .get_possible_values()
                .iter()
                .map(|value| value.get_name())
                .collect::<Vec<_>>()
                .join("、");
            let defaults = arg
                .get_default_values()
                .iter()
                .map(|value| value.to_string_lossy())
                .collect::<Vec<_>>()
                .join("、");
            let mut help = arg.get_help().map(ToString::to_string).unwrap_or_default();
            if !choices.is_empty() {
                help.push_str(&format!("（可选：{choices}）"));
            }
            if !defaults.is_empty()
                && !matches!(
                    arg.get_action(),
                    clap::ArgAction::SetTrue | clap::ArgAction::SetFalse
                )
            {
                help.push_str(&format!("（默认：{defaults}）"));
            }
            let arg = arg
                .help_heading(heading)
                .hide_possible_values(true)
                .hide_default_value(true)
                .help(help);
            match arg.get_id().as_str() {
                "help" => arg.help("显示帮助"),
                "version" => arg.help("显示版本"),
                "command" if arg.is_positional() => arg.help("需要查看帮助的命令"),
                _ => arg,
            }
        });
    let usage = command
        .render_usage()
        .to_string()
        .replacen("Usage: ", "", 1)
        .replace("[OPTIONS]", "[选项]");
    command = command.override_usage(usage);
    if command.get_name() == "help" {
        command = command.about("显示命令帮助");
    }
    for child in command.get_subcommands_mut() {
        *child = localize(child.clone());
    }
    command
}

pub fn command<T: CommandFactory>() -> Command {
    let mut command = T::command();
    command.build();
    localize(command)
}

pub fn parse<T: Parser>() -> T {
    let mut command = command::<T>();
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() == 1 {
        let _ = command.print_help();
        println!();
        std::process::exit(0);
    }
    let parsed = command
        .clone()
        .try_get_matches_from(args)
        .and_then(|matches| T::from_arg_matches(&matches));
    match parsed {
        Ok(value) => value,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            let _ = error.print();
            std::process::exit(0);
        }
        Err(error) => {
            // Clap 的原错误可能包含误填的密码、密钥或文件名，只呈现错误类别和声明的用法。
            let message = match error.kind() {
                ErrorKind::MissingRequiredArgument => "缺少必填参数，请查看此命令的 --help。",
                ErrorKind::ArgumentConflict => "参数重复或互相冲突，请查看此命令的 --help。",
                ErrorKind::InvalidValue | ErrorKind::ValueValidation => {
                    "参数值或格式无效，请查看允许的取值。"
                }
                ErrorKind::UnknownArgument | ErrorKind::InvalidSubcommand => {
                    "未知命令或选项，请查看 --help。"
                }
                _ => "命令参数不完整或格式有误，请查看 --help。",
            };
            let usage = error
                .get(clap::error::ContextKind::Usage)
                .map(ToString::to_string)
                .unwrap_or_else(|| command.render_usage().to_string());
            eprintln!(
                "错误：{message}\n\n{}",
                usage.replacen("Usage:", "用法：", 1)
            );
            std::process::exit(2);
        }
    }
}
