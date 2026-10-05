//! The `order_status` Spark; its view is `resources/views/sparks/order_status.mold.html`. It listens to `App\Events\OrderShipped`
//! on `private-orders.{order_id}` (Anvil) and updates without JavaScript.

use serde::{Deserialize, Serialize};
use smeltery::prelude::*;

/// A live component that listens to broadcasts: `@spark("order_status")` shows it on a page.
#[derive(Serialize, Deserialize, Default, Spark)]
#[spark(name = "order_status", stream)]
pub struct OrderStatus {
    /// Names the channel (`{order_id}`); set it in `mount`, from the page's props or the signed-in user.
    pub order_id: i64,
    /// How many events arrived since the page opened.
    pub received: i64,
}

/// The data of `App\Events\OrderShipped`: add the fields of the event this Spark reads (others are ignored).
#[derive(Debug, Deserialize)]
pub struct OrderShipped {}

#[actions]
impl OrderStatus {
    /// Runs on the first render: `@spark("order_status", { order_id: … })`.
    pub async fn mount(&mut self, ctx: &mut SparkCtx) -> Result<()> {
        self.order_id = ctx.prop("order_id").unwrap_or_default();
        Ok(())
    }

    /// Runs when `App\Events\OrderShipped` is broadcast on `private-orders.{order_id}` (the channel rules of `routes/channels.rs` decide
    /// whether this visitor receives it).
    #[on("anvil:private-orders.{order_id}", "App\\Events\\OrderShipped")]
    pub async fn received(&mut self, _event: OrderShipped) -> Result<()> {
        self.received += 1;
        Ok(())
    }
}
