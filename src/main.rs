mod cli;
mod spec;

use clap::Parser;

fn main() {
    let cli = cli::Cli::parse();
    println!("{cli:#?}");
}
