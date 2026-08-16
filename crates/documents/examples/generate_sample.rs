use chrono::{NaiveDate, Utc};
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use mi_rectificacion_documents::{ApplicantDetails, EvidenceAsset, generate_bundle};
use mi_rectificacion_domain::{
    EvidenceDocument, EvidenceKind, ExchangeRateSnapshot, ProductDraft, ProductLine,
    RectificationCase,
};
use printpdf::{
    FontId, Mm, Op, ParsedFont, PdfDocument, PdfPage, PdfSaveOptions, Point, Pt, TextItem,
};
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};
use std::{io::Cursor, path::PathBuf, str::FromStr};
use uuid::Uuid;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("output/pdf"));
    let case = RectificationCase::new(
        "RR123456789MX",
        Some("BOLETA-DEMO-2026".to_owned()),
        Some("Caso patrón - consola importada".to_owned()),
    )?;
    let products = vec![
        product(
            &case,
            "Consola portátil",
            "JPY",
            "32500.50",
            1,
            "2500",
            "0.10688",
        )?,
        product(
            &case,
            "Estuche protector",
            "USD",
            "19.99",
            2,
            "12.99",
            "18.5432",
        )?,
    ];
    let evidence = vec![
        image_evidence(
            &case,
            "Captura vertical de transacción",
            "transaccion_vertical.png",
            800,
            1400,
            [53, 95, 74],
        )?,
        image_evidence(
            &case,
            "Captura horizontal de producto",
            "producto_horizontal.png",
            1400,
            800,
            [69, 75, 104],
        )?,
        pdf_evidence(&case, "Estado de cuenta multipágina", "estado_cuenta.pdf")?,
    ];
    let applicant = ApplicantDetails {
        full_name: "Persona solicitante de ejemplo".to_owned(),
        email: "solicitante@example.com".to_owned(),
        phone: "+52 000 000 0000".to_owned(),
        address: "Domicilio de ejemplo, México".to_owned(),
        authority_name: "Autoridad aduanera competente".to_owned(),
        authority_email: "aduana@example.gob.mx".to_owned(),
        presumptive_value_mxn: "6055.87".to_owned(),
        city: "Puebla".to_owned(),
        state: "Puebla".to_owned(),
        postal_code: "72000".to_owned(),
        issuance_date: "14 de agosto de 2026".to_owned(),
        non_commercial_statement: "Los artículos son para uso personal y colección privada, sin fines de comercialización.".to_owned(),
        request_notes: "Los datos de este caso patrón son sintéticos y sirven únicamente para revisar el formato antes de generar un expediente real.".to_owned(),
        usd_rate: ExchangeRateSnapshot::automatic(
            "USD",
            NaiveDate::from_ymd_opt(2026, 8, 14).unwrap(),
            Decimal::from_str("18.5432")?,
            "Fuente de prueba",
            "https://example.test/usd-rate",
            Utc::now(),
        )?,
    };
    let bundle = generate_bundle(&case, &applicant, &products, &evidence, &output)?;
    println!("{}", bundle.directory.display());
    Ok(())
}

fn product(
    case: &RectificationCase,
    name: &str,
    currency: &str,
    price: &str,
    quantity: u32,
    shipping: &str,
    rate: &str,
) -> Result<ProductLine, Box<dyn std::error::Error>> {
    let draft = ProductDraft::new(
        name,
        None,
        quantity,
        Decimal::from_str(price)?,
        Decimal::ZERO,
        Decimal::from_str(shipping)?,
        Decimal::ZERO,
        currency,
    )?;
    let snapshot = ExchangeRateSnapshot::automatic(
        currency,
        NaiveDate::from_ymd_opt(2026, 8, 14).unwrap(),
        Decimal::from_str(rate)?,
        "Fuente de prueba",
        "https://example.test/rate",
        Utc::now(),
    )?;
    Ok(ProductLine::new(case.id, draft, snapshot)?)
}

fn image_evidence(
    case: &RectificationCase,
    title: &str,
    filename: &str,
    width: u32,
    height: u32,
    base: [u8; 3],
) -> Result<EvidenceAsset, Box<dyn std::error::Error>> {
    let mut image = RgbImage::new(width, height);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let stripe = if (x / 80 + y / 80) % 2 == 0 { 22 } else { 0 };
        *pixel = Rgb([
            base[0].saturating_add(stripe),
            base[1].saturating_add(stripe),
            base[2].saturating_add(stripe),
        ]);
    }
    let mut bytes = Vec::new();
    DynamicImage::ImageRgb8(image).write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)?;
    Ok(asset(case, title, filename, "image/png", bytes))
}

fn pdf_evidence(
    case: &RectificationCase,
    title: &str,
    filename: &str,
) -> Result<EvidenceAsset, Box<dyn std::error::Error>> {
    let mut pdf = PdfDocument::new("Estado de cuenta sintético");
    let font = ParsedFont::from_bytes(
        include_bytes!("../../../assets/fonts/Roboto-Medium.ttf"),
        0,
        &mut Vec::new(),
    )
    .ok_or("No se pudo cargar la fuente Unicode de prueba")?;
    let font_id = pdf.add_font(&font);
    let pages = vec![
        synthetic_statement_page(1, &font_id),
        synthetic_statement_page(2, &font_id),
    ];
    let bytes = pdf
        .with_pages(pages)
        .save(&PdfSaveOptions::default(), &mut Vec::new());
    Ok(asset(case, title, filename, "application/pdf", bytes))
}

fn synthetic_statement_page(number: usize, font: &FontId) -> PdfPage {
    PdfPage::new(
        Mm(210.0),
        Mm(297.0),
        vec![
            Op::StartTextSection,
            Op::SetTextCursor {
                pos: Point::new(Mm(28.0), Mm(250.0)),
            },
            Op::SetFontSize {
                size: Pt(22.0),
                font: font.clone(),
            },
            Op::WriteText {
                items: vec![TextItem::Text(format!(
                    "ESTADO DE CUENTA SINTÉTICO - PÁGINA {number}"
                ))],
                font: font.clone(),
            },
            Op::EndTextSection,
        ],
    )
}

fn asset(
    case: &RectificationCase,
    title: &str,
    filename: &str,
    content_type: &str,
    bytes: Vec<u8>,
) -> EvidenceAsset {
    let sha256 = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    EvidenceAsset {
        document: EvidenceDocument {
            id: Uuid::new_v4(),
            case_id: case.id,
            kind: EvidenceKind::Transaction,
            title: title.to_owned(),
            original_filename: filename.to_owned(),
            content_type: content_type.to_owned(),
            size_bytes: bytes.len() as u64,
            sha256,
            encrypted_relative_path: String::new(),
            order_index: 0,
            created_at: Utc::now(),
        },
        bytes,
    }
}
