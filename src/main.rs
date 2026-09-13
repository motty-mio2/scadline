mod application;
mod domain;
mod infrastructure;
mod presentation;

fn main() -> eframe::Result<()> {
    presentation::run()
}
