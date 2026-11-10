//! Commands added after the first two phases.

use crate::Opened;
use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum Command {}

pub async fn run(_o: &Opened, cmd: Command) -> Result<()> {
    match cmd {}
}
