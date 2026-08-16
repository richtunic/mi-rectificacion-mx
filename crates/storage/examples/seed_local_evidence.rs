use mi_rectificacion_application::CaseRepository;
use mi_rectificacion_domain::EvidenceKind;
use mi_rectificacion_storage::{EvidenceVault, SqliteCaseRepository};
use std::{
    io::{self, Read},
    path::PathBuf,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("Indica la ruta del archivo de prueba")?;
    let mut tracking_number = String::new();
    io::stdin().read_to_string(&mut tracking_number)?;
    let tracking_number = tracking_number.trim();

    let repository = SqliteCaseRepository::open_default()?;
    let case = repository
        .list()?
        .into_iter()
        .find(|case| case.tracking_number == tracking_number)
        .ok_or("No se encontró el expediente local")?;
    EvidenceVault::open_default()?.import_evidence(
        case.id,
        EvidenceKind::Other,
        Some("Prueba técnica cifrada".to_owned()),
        &source,
    )?;

    println!("LOCAL_ENCRYPTED_EVIDENCE_CREATED");
    Ok(())
}
