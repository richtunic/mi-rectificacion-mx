use chrono::NaiveDate;
use mi_rectificacion_integrations::{ExchangeRateProvider, FrankfurterProvider};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let currency = arguments.next().unwrap_or_else(|| "JPY".to_owned());
    let date = arguments
        .next()
        .map(|value| NaiveDate::parse_from_str(&value, "%Y-%m-%d"))
        .transpose()?
        .unwrap_or_else(|| chrono::Local::now().date_naive());
    let snapshot = FrankfurterProvider::new()?.rate_to_mxn(&currency, date)?;

    println!(
        "1 {} = {} MXN | fecha efectiva: {} | fuente: {} | {}",
        snapshot.currency,
        snapshot.rate_to_mxn,
        snapshot.rate_date,
        snapshot.source_name,
        snapshot.source_url
    );
    Ok(())
}
