use chrono::{Datelike, Local};
use mi_rectificacion_integrations::{CorreosMexicoProvider, TrackingProvider};
use std::io::{self, Read};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let year = std::env::args()
        .nth(1)
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or_else(|| Local::now().year());
    let mut tracking_number = String::new();
    io::stdin().read_to_string(&mut tracking_number)?;
    let response = CorreosMexicoProvider::new()?.track(tracking_number.trim(), year)?;
    println!(
        "Consulta completada: {} movimiento(s)",
        response.events.len()
    );
    Ok(())
}
