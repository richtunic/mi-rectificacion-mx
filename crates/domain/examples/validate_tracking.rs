use mi_rectificacion_domain::normalize_tracking_number;
use std::io::{self, Read};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    normalize_tracking_number(&input)?;
    println!("VALID_TRACKING_NUMBER");
    Ok(())
}
