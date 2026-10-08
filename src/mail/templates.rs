//! Rendering the shop's emails. Templates are compiled into the binary; the
//! HTML one is auto-escaped, so names from customers or products can't inject
//! markup.

use std::sync::LazyLock;

use minijinja::{Environment, context};
use serde::{Deserialize, Serialize};

use super::Email;
use crate::checkout::orders::{OrderStatus, OrderView};

/// Which order email to send.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderEmail {
    /// "We've received your order."
    Placed,
    /// "We've received your payment."
    Paid,
}

static TEMPLATES: LazyLock<Environment<'static>> = LazyLock::new(|| {
    let mut env = Environment::new();
    env.add_template("order_placed.txt", include_str!("templates/order_placed.txt"))
        .expect("order_placed.txt is valid");
    env.add_template("order_paid.txt", include_str!("templates/order_paid.txt"))
        .expect("order_paid.txt is valid");
    env.add_template("order.html", include_str!("templates/order.html"))
        .expect("order.html is valid");
    env
});

/// "24 hours", "90 minutes".
fn duration_words(minutes: i64) -> String {
    if minutes >= 60 && minutes % 60 == 0 {
        let h = minutes / 60;
        format!("{h} hour{}", if h == 1 { "" } else { "s" })
    } else {
        format!("{minutes} minute{}", if minutes == 1 { "" } else { "s" })
    }
}

/// Renders an order email.
///
/// # Errors
/// Only if a template is broken, which tests catch.
pub fn render_order_email(
    kind: OrderEmail,
    order: &OrderView,
    shop: &str,
    payment_window_minutes: i64,
) -> Result<Email, minijinja::Error> {
    let a = &order.shipping_address;
    let address = [
        &a.name,
        &a.line1,
        &a.line2,
        &format!("{} {}", a.postcode, a.city).trim().to_owned(),
        &a.state,
        &a.country,
    ]
    .iter()
    .filter(|s| !s.is_empty())
    .map(|s| s.as_str())
    .collect::<Vec<_>>()
    .join("\n");
    let lines: Vec<_> = order
        .lines
        .iter()
        .map(|l| {
            let name = if l.sku_name.is_empty() {
                l.product_name.clone()
            } else {
                format!("{} ({})", l.product_name, l.sku_name)
            };
            context! { name, quantity => l.quantity, subtotal => l.subtotal.to_string() }
        })
        .collect();
    let ctx = context! {
        shop,
        name => a.name,
        number => order.number,
        lines,
        subtotal => order.subtotal.to_string(),
        shipping_name => order.shipping.name,
        shipping => order.shipping.price.to_string(),
        tax_name => order.tax.name,
        tax => order.tax.amount.to_string(),
        tax_included => order.tax.prices_include_tax,
        total => order.total.to_string(),
        address,
        paid => kind == OrderEmail::Paid,
        awaiting_payment => order.status == OrderStatus::PendingPayment,
        pay_within => duration_words(payment_window_minutes),
    };
    let (text_template, subject) = match kind {
        OrderEmail::Placed => ("order_placed.txt", format!("Order #{} received", order.number)),
        OrderEmail::Paid => (
            "order_paid.txt",
            format!("Payment received for order #{}", order.number),
        ),
    };
    Ok(Email {
        to: order.email.clone(),
        subject: format!("{subject} · {shop}"),
        text: TEMPLATES.get_template(text_template)?.render(&ctx)?,
        html: TEMPLATES.get_template("order.html")?.render(&ctx)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration_words(1440), "24 hours");
        assert_eq!(duration_words(60), "1 hour");
        assert_eq!(duration_words(90), "90 minutes");
        assert_eq!(duration_words(1), "1 minute");
    }
}
