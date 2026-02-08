use crate::v1::run;
use anyhow::Result;

mod v0;
mod v1;

fn main() -> Result<()> {
    run()
}
