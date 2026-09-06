use std::fmt::Display;

use anyhow::Result;
use async_trait::async_trait;

use super::context::Ctx;

#[async_trait]
pub trait Action: Display {
    async fn execute(&self, ctx: Ctx) -> Result<()>;
}
