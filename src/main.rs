mod application;
mod cli;
mod domain;
mod infrastructure;
mod presentation;

fn main() -> eframe::Result<()> {
    match cli::parse_args(std::env::args_os().skip(1)) {
        Ok(cli::CliAction::Run(options)) => presentation::run(options),
        Ok(cli::CliAction::Help) => {
            print!("{}", cli::HELP);
            Ok(())
        }
        Err(error) => {
            eprintln!("エラー: {error}\n\n{}", cli::HELP);
            std::process::exit(2);
        }
    }
}
