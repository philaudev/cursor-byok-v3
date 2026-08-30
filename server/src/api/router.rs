//! Builds the top-level server router.

use crate::{cursor::transport::TransportRegistry, Result};

pub fn router(registry: TransportRegistry) -> Result<axum::Router> {
    super::cursor::router(registry)
}
