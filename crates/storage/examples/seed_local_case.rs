use mi_rectificacion_application::{CreateCaseInput, create_case};
use mi_rectificacion_storage::SqliteCaseRepository;
use std::io::{self, Read};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut tracking_number = String::new();
    io::stdin().read_to_string(&mut tracking_number)?;

    let repository = SqliteCaseRepository::open_default()?;
    create_case(
        &repository,
        CreateCaseInput {
            display_name: Some("Prueba internacional Japón".to_owned()),
            tracking_number,
            customs_form_number: None,
        },
    )?;

    println!("LOCAL_CASE_CREATED");
    Ok(())
}
