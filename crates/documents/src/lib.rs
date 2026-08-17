use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, Utc};
use lopdf::{Document as LoDocument, Object, dictionary};
use mi_rectificacion_domain::{
    EvidenceDocument, ExchangeRateSnapshot, ProductLine, RectificationCase,
    calculate_customs_overvaluation,
};
use printpdf::{
    BuiltinFont, Color, FontId, Mm, Op, ParsedFont, PdfDocument, PdfPage, PdfSaveOptions, Point,
    Pt, RawImage, Rgb, TextItem, XObjectTransform,
};
use rust_decimal::{Decimal, RoundingStrategy};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Cursor, Write},
    path::{Path, PathBuf},
    str::FromStr,
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

pub const TEMPLATE_VERSION: &str = "2026.08-v9";
const ROBOTO_MEDIUM: &[u8] = include_bytes!("../../../assets/fonts/Roboto-Medium.ttf");
const POSTAL_EXEMPTION_LIMIT_USD: Decimal = Decimal::from_parts(50, 0, 0, false, 0);
const POSTAL_SIMPLIFIED_LIMIT_USD: Decimal = Decimal::from_parts(1000, 0, 0, false, 0);
const POSTAL_GLOBAL_RATE: Decimal = Decimal::from_parts(19, 0, 0, false, 2);

#[derive(Debug, Clone)]
pub struct ApplicantDetails {
    pub full_name: String,
    pub email: String,
    pub phone: String,
    pub address: String,
    pub authority_name: String,
    pub authority_email: String,
    pub presumptive_value_mxn: String,
    pub city: String,
    pub state: String,
    pub postal_code: String,
    pub issuance_date: String,
    pub non_commercial_statement: String,
    pub request_notes: String,
    pub usd_rate: ExchangeRateSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PostalTaxTreatment {
    Exempt,
    GlobalRate19,
    OutsideSimplifiedProcedure,
}

#[derive(Debug, Clone, Copy)]
struct PostalAssessment {
    total_mxn: Decimal,
    usd_equivalent: Decimal,
    tax_mxn: Option<Decimal>,
    treatment: PostalTaxTreatment,
}

#[derive(Debug, Clone)]
pub struct EvidenceAsset {
    pub document: EvidenceDocument,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct GeneratedBundle {
    pub directory: PathBuf,
    pub request_pdf: PathBuf,
    pub evidence_pdf: PathBuf,
    pub print_pdf: PathBuf,
    pub request_docx: PathBuf,
    pub email_draft: PathBuf,
    pub manifest: PathBuf,
    pub zip: PathBuf,
    pub email_content: EmailContent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailContent {
    pub recipient: String,
    pub sender: String,
    pub subject: String,
    pub body: String,
}

#[derive(Serialize)]
struct BundleManifest<'a> {
    format_version: u8,
    template_version: &'static str,
    generated_at: DateTime<Utc>,
    case_id: String,
    tracking_number: &'a str,
    products: usize,
    evidence: Vec<ManifestEvidence<'a>>,
    files: Vec<ManifestFile>,
}

#[derive(Serialize)]
struct ManifestEvidence<'a> {
    title: &'a str,
    filename: &'a str,
    content_type: &'a str,
    sha256: &'a str,
}

#[derive(Serialize)]
struct ManifestFile {
    filename: String,
    sha256: String,
}

pub fn generate_bundle(
    case: &RectificationCase,
    applicant: &ApplicantDetails,
    products: &[ProductLine],
    evidence: &[EvidenceAsset],
    destination: &Path,
) -> Result<GeneratedBundle> {
    validate_applicant(applicant)?;
    if products.is_empty() {
        bail!("Agrega al menos un producto valorado antes de generar el escrito");
    }
    fs::create_dir_all(destination)
        .with_context(|| format!("No se pudo crear {}", destination.display()))?;
    let directory = destination.join(format!("Expediente-{}", case.tracking_number));
    fs::create_dir_all(&directory)
        .with_context(|| format!("No se pudo crear {}", directory.display()))?;

    let request_pdf = directory.join("01_solicitud_rectificacion.pdf");
    let evidence_pdf = directory.join("02_dossier_pruebas.pdf");
    let print_pdf = directory.join("03_expediente_listo_para_imprimir.pdf");
    let request_docx = directory.join("04_solicitud_rectificacion_editable.docx");
    let email_draft = directory.join("05_correo_aduanas.eml");
    let manifest = directory.join("manifest.json");
    let zip = directory.join(format!("expediente-{}.zip", case.tracking_number));

    let request_bytes = build_request_pdf(case, applicant, products);
    write_atomic(&request_pdf, &request_bytes)?;
    let evidence_bytes = build_evidence_pdf(case, products, evidence)?;
    write_atomic(&evidence_pdf, &evidence_bytes)?;
    let print_bytes = merge_pdf_documents(&[&request_bytes, &evidence_bytes])?;
    write_atomic(&print_pdf, &print_bytes)?;
    let request_docx_bytes = build_request_docx(case, applicant, products)?;
    write_atomic(&request_docx, &request_docx_bytes)?;
    let email_content = compose_email(case, applicant, products);
    let email_bytes = build_email_message(
        &email_content,
        &[
            ("01_solicitud_rectificacion.pdf", request_bytes.as_slice()),
            ("02_dossier_pruebas.pdf", evidence_bytes.as_slice()),
        ],
    )
    .into_bytes();
    write_atomic(&email_draft, &email_bytes)?;

    let file_entries = vec![
        ManifestFile {
            filename: file_name(&request_pdf)?,
            sha256: sha256_hex(&request_bytes),
        },
        ManifestFile {
            filename: file_name(&evidence_pdf)?,
            sha256: sha256_hex(&evidence_bytes),
        },
        ManifestFile {
            filename: file_name(&print_pdf)?,
            sha256: sha256_hex(&print_bytes),
        },
        ManifestFile {
            filename: file_name(&request_docx)?,
            sha256: sha256_hex(&request_docx_bytes),
        },
        ManifestFile {
            filename: file_name(&email_draft)?,
            sha256: sha256_hex(&email_bytes),
        },
    ];
    let manifest_value = BundleManifest {
        format_version: 1,
        template_version: TEMPLATE_VERSION,
        generated_at: Utc::now(),
        case_id: case.id.to_string(),
        tracking_number: &case.tracking_number,
        products: products.len(),
        evidence: evidence
            .iter()
            .map(|asset| ManifestEvidence {
                title: &asset.document.title,
                filename: &asset.document.original_filename,
                content_type: &asset.document.content_type,
                sha256: &asset.document.sha256,
            })
            .collect(),
        files: file_entries,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest_value)?;
    write_atomic(&manifest, &manifest_bytes)?;
    create_zip(
        &zip,
        &[
            (&request_pdf, request_bytes.as_slice()),
            (&evidence_pdf, evidence_bytes.as_slice()),
            (&print_pdf, print_bytes.as_slice()),
            (&request_docx, request_docx_bytes.as_slice()),
            (&email_draft, email_bytes.as_slice()),
            (&manifest, manifest_bytes.as_slice()),
        ],
        evidence,
    )?;

    Ok(GeneratedBundle {
        directory,
        request_pdf,
        evidence_pdf,
        print_pdf,
        request_docx,
        email_draft,
        manifest,
        zip,
        email_content,
    })
}

pub fn export_print_ready_pdf(
    case: &RectificationCase,
    applicant: &ApplicantDetails,
    products: &[ProductLine],
    evidence: &[EvidenceAsset],
    destination: &Path,
) -> Result<PathBuf> {
    validate_applicant(applicant)?;
    if products.is_empty() {
        bail!("Agrega al menos un producto valorado antes de exportar el expediente");
    }
    let request_bytes = build_request_pdf(case, applicant, products);
    let evidence_bytes = build_evidence_pdf(case, products, evidence)?;
    let print_bytes = merge_pdf_documents(&[&request_bytes, &evidence_bytes])?;
    write_atomic(destination, &print_bytes)?;
    Ok(destination.to_path_buf())
}

pub fn export_editable_docx(
    case: &RectificationCase,
    applicant: &ApplicantDetails,
    products: &[ProductLine],
    destination: &Path,
) -> Result<PathBuf> {
    validate_applicant(applicant)?;
    if products.is_empty() {
        bail!("Agrega al menos un producto valorado antes de exportar el expediente");
    }
    let request_docx_bytes = build_request_docx(case, applicant, products)?;
    write_atomic(destination, &request_docx_bytes)?;
    Ok(destination.to_path_buf())
}

fn validate_applicant(applicant: &ApplicantDetails) -> Result<()> {
    if applicant.full_name.trim().is_empty() {
        bail!("El nombre de la persona solicitante es obligatorio");
    }
    if applicant.email.trim().is_empty() || !applicant.email.contains('@') {
        bail!("Captura un correo electrónico válido");
    }
    if applicant.authority_name.trim().is_empty() {
        bail!("La autoridad destinataria es obligatoria");
    }
    if applicant.authority_email.trim().is_empty() || !applicant.authority_email.contains('@') {
        bail!("Captura un correo válido para la autoridad destinataria");
    }
    let presumptive_value = parse_mxn(&applicant.presumptive_value_mxn)
        .context("Captura el valor presuntivo de la boleta en MXN")?;
    if presumptive_value <= Decimal::ZERO {
        bail!("El valor presuntivo debe ser mayor que cero");
    }
    if applicant.city.trim().is_empty() {
        bail!("La ciudad del solicitante es obligatoria");
    }
    if applicant.state.trim().is_empty() {
        bail!("El estado del solicitante es obligatorio");
    }
    if applicant.postal_code.trim().len() != 5
        || !applicant
            .postal_code
            .trim()
            .bytes()
            .all(|byte| byte.is_ascii_digit())
    {
        bail!("El código postal debe tener cinco dígitos");
    }
    if applicant.issuance_date.trim().is_empty() {
        bail!("La fecha del escrito es obligatoria");
    }
    if applicant.usd_rate.currency != "USD" || applicant.usd_rate.rate_to_mxn <= Decimal::ZERO {
        bail!("La tasa USD/MXN para evaluar el umbral postal no es válida");
    }
    Ok(())
}

fn postal_assessment(
    products: &[ProductLine],
    usd_rate: &ExchangeRateSnapshot,
) -> PostalAssessment {
    let total_mxn: Decimal = products.iter().map(|product| product.total_mxn).sum();
    let usd_equivalent = (total_mxn / usd_rate.rate_to_mxn)
        .round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero);
    let (treatment, tax_mxn) = if usd_equivalent <= POSTAL_EXEMPTION_LIMIT_USD {
        (PostalTaxTreatment::Exempt, Some(Decimal::ZERO))
    } else if usd_equivalent <= POSTAL_SIMPLIFIED_LIMIT_USD {
        (
            PostalTaxTreatment::GlobalRate19,
            Some(
                (total_mxn * POSTAL_GLOBAL_RATE)
                    .round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero),
            ),
        )
    } else {
        (PostalTaxTreatment::OutsideSimplifiedProcedure, None)
    };
    PostalAssessment {
        total_mxn,
        usd_equivalent,
        tax_mxn,
        treatment,
    }
}

fn build_request_pdf(
    case: &RectificationCase,
    applicant: &ApplicantDetails,
    products: &[ProductLine],
) -> Vec<u8> {
    let mut document = PdfDocument::new("Solicitud de rectificación aduanal");
    let unicode_font = ParsedFont::from_bytes(ROBOTO_MEDIUM, 0, &mut Vec::new())
        .expect("La fuente Unicode incluida debe ser válida");
    let unicode_font_id = document.add_font(&unicode_font);
    let folio = case
        .customs_form_number
        .as_deref()
        .unwrap_or("No capturado");
    let presumptive = parse_mxn(&applicant.presumptive_value_mxn).unwrap_or(Decimal::ZERO);
    let assessment = postal_assessment(products, &applicant.usd_rate);
    let difference = (presumptive - assessment.total_mxn).max(Decimal::ZERO);
    let overvaluation = calculate_customs_overvaluation(presumptive, assessment.total_mxn);
    let merchandise = products
        .iter()
        .map(|product| {
            let unit = if product.quantity == 1 {
                "pieza"
            } else {
                "piezas"
            };
            format!("{} ({} {unit})", product.name, product.quantity)
        })
        .collect::<Vec<_>>()
        .join(", ");
    let original_values = products
        .iter()
        .map(|product| format!("{:.2} {}", product.total_original, product.currency))
        .collect::<Vec<_>>()
        .join("; ");

    let mut first = Vec::new();
    unicode_text(
        &mut first,
        22.0,
        280.0,
        10.0,
        &unicode_font_id,
        "ASUNTO: Solicitud de rectificación de valoración aduanera por determinación",
    );
    unicode_text(
        &mut first,
        22.0,
        273.0,
        10.0,
        &unicode_font_id,
        "presuntiva excesiva.",
    );
    unicode_text(
        &mut first,
        22.0,
        259.0,
        10.0,
        &unicode_font_id,
        "C. ADMINISTRADOR DE LA ADUANA",
    );
    unicode_text(
        &mut first,
        22.0,
        252.0,
        10.0,
        &unicode_font_id,
        "ATO CIUDAD DE MÉXICO",
    );
    unicode_text(
        &mut first,
        22.0,
        245.0,
        10.0,
        &unicode_font_id,
        "OFICINA DE INTERCAMBIO POSTAL",
    );
    unicode_text(
        &mut first,
        22.0,
        238.0,
        10.0,
        &unicode_font_id,
        "P R E S E N T E",
    );
    let appearance = format!(
        "{}, por mi propio derecho, señalando como medios de contacto {} y {}, respetuosamente comparezco para exponer:",
        applicant.full_name,
        applicant.email,
        value_or_dash(&applicant.phone)
    );
    unicode_wrapped_text(
        &mut first,
        22.0,
        220.0,
        166.0,
        9.0,
        5.0,
        &unicode_font_id,
        &appearance,
    );
    section_title(&mut first, 198.0, &unicode_font_id, "I. OBJETO");
    let object = format!(
        "Por medio del presente escrito, solicito la rectificación de la valoración aduanera efectuada al envío identificado con número de guía {} y boleta aduanal {}, toda vez que se determinó un valor presuntivo de ${:.2} M.N., el cual no corresponde al precio realmente pagado por la mercancía.",
        case.tracking_number, folio, presumptive
    );
    unicode_wrapped_text(
        &mut first,
        22.0,
        189.0,
        166.0,
        8.7,
        4.8,
        &unicode_font_id,
        &object,
    );
    section_title(&mut first, 157.0, &unicode_font_id, "II. HECHOS Y CÁLCULO");
    let facts = [
        format!(
            "1. La mercancía consiste en {}.",
            truncate(&merchandise, 190)
        ),
        format!(
            "2. El valor real de transacción acreditado es de {original_values}, equivalente a ${:.2} M.N. conforme a las tasas documentadas en el dossier.",
            assessment.total_mxn
        ),
        format!(
            "3. Para evaluar el umbral postal, el valor real equivale a ${:.2} USD usando 1 USD = {:.4} MXN, tasa de {} con fecha {}. Esta referencia debe confirmarse contra el tipo de cambio aduanero aplicable.",
            assessment.usd_equivalent,
            applicant.usd_rate.rate_to_mxn,
            applicant.usd_rate.source_name,
            applicant.usd_rate.rate_date
        ),
        overvaluation.map_or_else(
            || format!(
                "4. La boleta consigna ${:.2} M.N.; no se identifica un exceso positivo frente al valor real acreditado de ${:.2} M.N., por lo que se solicita revisar la discrepancia con base en la evidencia aportada.",
                presumptive, assessment.total_mxn
            ),
            |comparison| format!(
                "4. La boleta consigna ${:.2} M.N., es decir, una valuación ${:.2} M.N. superior al valor real acreditado, equivalente a un exceso de {:.2}%. Por tal motivo, respetuosamente solicito que esta diferencia sea revisada y, de estimarse procedente, se ajuste la valoración con base en la evidencia aportada.",
                presumptive,
                difference,
                comparison.percentage_above_real_value
            ),
        ),
    ];
    let mut y = 147.0;
    for fact in facts {
        y = unicode_wrapped_text(
            &mut first,
            27.0,
            y,
            158.0,
            8.4,
            4.6,
            &unicode_font_id,
            &fact,
        ) - 3.0;
    }
    section_title(
        &mut first,
        y - 1.0,
        &unicode_font_id,
        "III. FUNDAMENTO LEGAL",
    );
    unicode_wrapped_text(
        &mut first,
        22.0,
        y - 11.0,
        166.0,
        8.4,
        4.6,
        &unicode_font_id,
        "Conforme a los artículos 64 y 65 de la Ley Aduanera, la valoración debe partir del valor de transacción acreditado y no de una estimación presuntiva desproporcionada o ajena al precio efectivamente pagado.",
    );
    page_footer(&mut first, 1, 2);

    let mut second = Vec::new();
    let mut y = unicode_wrapped_text(
        &mut second,
        22.0,
        275.0,
        166.0,
        8.7,
        4.8,
        &unicode_font_id,
        "La RGCE 2026, regla 3.7.2, fracción I, dispone para envíos por vía postal el despacho sin pago de IGI, IVA ni DTA cuando el valor en aduana es igual o menor a 50 USD y la mercancía no está sujeta a regulaciones o restricciones no arancelarias.",
    );
    y -= 4.0;
    y = unicode_wrapped_text(
        &mut second,
        22.0,
        y,
        166.0,
        8.7,
        4.8,
        &unicode_font_id,
        "La fracción II de la misma regla permite, para valores mayores a 50 USD y hasta 1,000 USD, utilizar el Formulario Postal D1 aplicando una tasa global del 19%, salvo mercancías sujetas a tasas específicas u otros supuestos legales.",
    );
    y -= 4.0;
    let tax_conclusion = match assessment.treatment {
        PostalTaxTreatment::Exempt => format!(
            "Tomando como referencia el tipo de cambio documentado, el valor acreditado equivale a ${:.2} USD y se ubica por debajo del umbral de 50 USD. Por tal motivo, respetuosamente solicito que se verifique si resulta aplicable el supuesto previsto en la regla citada y, en su caso, se ajusten las contribuciones determinadas.",
            assessment.usd_equivalent
        ),
        PostalTaxTreatment::GlobalRate19 => format!(
            "En este caso, el valor acreditado de ${:.2} USD supera 50 USD y no excede 1,000 USD. El cálculo correcto bajo la tasa global es: ${:.2} M.N. x 19% = ${:.2} M.N.",
            assessment.usd_equivalent,
            assessment.total_mxn,
            assessment.tax_mxn.unwrap_or(Decimal::ZERO)
        ),
        PostalTaxTreatment::OutsideSimplifiedProcedure => format!(
            "El valor acreditado de ${:.2} USD excede el límite de 1,000 USD del procedimiento postal simplificado. Por ello no se presenta el 19% como importe definitivo y se solicita aplicar el procedimiento legal que corresponda sobre el valor real acreditado.",
            assessment.usd_equivalent
        ),
    };
    y = unicode_wrapped_text(
        &mut second,
        22.0,
        y,
        166.0,
        8.8,
        4.8,
        &unicode_font_id,
        &tax_conclusion,
    );
    y -= 4.0;
    let payment_position = match assessment.tax_mxn {
        Some(amount) if amount > Decimal::ZERO => format!(
            "No me niego al pago de las contribuciones legalmente procedentes y manifiesto mi disposición a cubrir ${amount:.2} M.N.; solicito únicamente que el cobro se calcule sobre el valor real comprobado y no sobre una valuación excesiva."
        ),
        _ => "Manifiesto mi disposición a cumplir las obligaciones fiscales que legalmente correspondan y solicito respetuosamente que cualquier contribución se determine de manera fundada sobre el valor real comprobado.".to_owned(),
    };
    y = unicode_wrapped_text(
        &mut second,
        22.0,
        y,
        166.0,
        8.7,
        4.8,
        &unicode_font_id,
        &payment_position,
    );
    y -= 8.0;
    section_title(
        &mut second,
        y,
        &unicode_font_id,
        "IV. NATURALEZA NO COMERCIAL",
    );
    y = unicode_wrapped_text(
        &mut second,
        22.0,
        y - 10.0,
        166.0,
        9.0,
        5.0,
        &unicode_font_id,
        value_or_default(
            &applicant.non_commercial_statement,
            "La mercancía corresponde a artículos en cantidad razonable para uso personal. No existe habitualidad, volumen comercial ni finalidad lucrativa que permita presumir actividad empresarial.",
        ),
    );
    if !applicant.request_notes.trim().is_empty() {
        y -= 3.0;
        y = unicode_wrapped_text(
            &mut second,
            22.0,
            y,
            166.0,
            8.5,
            4.8,
            &unicode_font_id,
            &format!(
                "Hechos adicionales: {}",
                truncate(applicant.request_notes.trim(), 420)
            ),
        );
    }
    y -= 7.0;
    section_title(&mut second, y, &unicode_font_id, "V. PETICIÓN");
    let third_petition = match assessment.treatment {
        PostalTaxTreatment::Exempt => "3. Se verifique la procedencia del tratamiento previsto para envíos de hasta 50 USD y, si se satisfacen los demás requisitos aplicables, se ajusten las contribuciones determinadas.".to_owned(),
        PostalTaxTreatment::GlobalRate19 => format!(
            "3. Se determine la tasa global del 19% sobre el valor real, por un importe de ${:.2} M.N., sujeto a confirmar que no exista una tasa específica aplicable.",
            assessment.tax_mxn.unwrap_or(Decimal::ZERO)
        ),
        PostalTaxTreatment::OutsideSimplifiedProcedure => "3. Se determine el procedimiento y las contribuciones legalmente aplicables sobre el valor real acreditado, sin emplear la tasa simplificada como importe definitivo.".to_owned(),
    };
    let fourth_petition = match assessment.treatment {
        PostalTaxTreatment::Exempt => "4. Con base en el resultado de esa revisión, se emita la rectificación del Formulario Postal que en derecho corresponda.".to_owned(),
        _ => match assessment.tax_mxn {
            Some(amount) => format!(
                "4. Se emita una Rectificación de Formulario Postal con importe de ${amount:.2} M.N. por los conceptos aquí calculados."
            ),
            None => "4. Se emita la rectificación correspondiente con el importe debidamente fundado y calculado.".to_owned(),
        },
    };
    let petitions = [
        format!(
            "1. Se revise la valoración presuntiva de ${:.2} M.N. y, de resultar procedente, se rectifique con base en la documentación aportada.",
            presumptive
        ),
        format!(
            "2. Se tenga por acreditado, previa revisión de las pruebas aportadas, el valor real de ${:.2} M.N.",
            assessment.total_mxn
        ),
        third_petition,
        fourth_petition,
    ];
    y -= 10.0;
    for petition in petitions {
        y = unicode_wrapped_text(
            &mut second,
            27.0,
            y,
            158.0,
            8.2,
            4.5,
            &unicode_font_id,
            &petition,
        ) - 2.5;
    }
    unicode_text(
        &mut second,
        22.0,
        59.0,
        9.0,
        &unicode_font_id,
        &format!(
            "{}, {}, C.P. {}, a {}.",
            applicant.city.trim(),
            applicant.state.trim(),
            applicant.postal_code.trim(),
            applicant.issuance_date.trim()
        ),
    );
    unicode_text(
        &mut second,
        22.0,
        44.0,
        9.5,
        &unicode_font_id,
        &applicant.full_name,
    );
    unicode_text(
        &mut second,
        22.0,
        36.0,
        8.5,
        &unicode_font_id,
        "Firma: ______________________________",
    );
    page_footer(&mut second, 2, 2);

    let pages = vec![
        PdfPage::new(Mm(210.0), Mm(297.0), first),
        PdfPage::new(Mm(210.0), Mm(297.0), second),
    ];
    save_pdf(&mut document, pages)
}

fn section_title(ops: &mut Vec<Op>, y: f32, font: &FontId, value: &str) {
    unicode_text(ops, 22.0, y, 10.0, font, value);
}

fn unicode_text(ops: &mut Vec<Op>, x: f32, y: f32, size: f32, font: &FontId, value: &str) {
    ops.extend([
        Op::StartTextSection,
        Op::SetTextCursor {
            pos: Point::new(Mm(x), Mm(y)),
        },
        Op::SetFontSize {
            size: Pt(size),
            font: font.clone(),
        },
        Op::SetFillColor {
            col: Color::Rgb(Rgb::new(0.13, 0.15, 0.13, None)),
        },
        Op::WriteText {
            items: vec![TextItem::Text(value.to_owned())],
            font: font.clone(),
        },
        Op::EndTextSection,
    ]);
}

#[allow(clippy::too_many_arguments)]
fn unicode_wrapped_text(
    ops: &mut Vec<Op>,
    x: f32,
    mut y: f32,
    width_mm: f32,
    size: f32,
    line_height_mm: f32,
    font: &FontId,
    value: &str,
) -> f32 {
    let max_chars = ((width_mm * 5.0) / size.max(1.0)).max(18.0) as usize;
    let mut line = String::new();
    for word in value.split_whitespace() {
        if !line.is_empty() && line.chars().count() + word.chars().count() + 1 > max_chars {
            unicode_text(ops, x, y, size, font, &line);
            y -= line_height_mm;
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        unicode_text(ops, x, y, size, font, &line);
        y -= line_height_mm;
    }
    y
}

#[derive(Debug, Clone, Copy)]
struct ImagePlacement {
    scale: f32,
    x_pt: f32,
    y_pt: f32,
}

fn fit_image_to_printable_area(
    width_px: usize,
    height_px: usize,
    dpi: f32,
    max_width_mm: f32,
    max_height_mm: f32,
    bottom_mm: f32,
) -> ImagePlacement {
    let width_pt = width_px as f32 * 72.0 / dpi;
    let height_pt = height_px as f32 * 72.0 / dpi;
    let max_width_pt = max_width_mm * 72.0 / 25.4;
    let max_height_pt = max_height_mm * 72.0 / 25.4;
    let scale = (max_width_pt / width_pt).min(max_height_pt / height_pt);
    let rendered_width_pt = width_pt * scale;
    let rendered_height_pt = height_pt * scale;

    ImagePlacement {
        scale,
        x_pt: (210.0 * 72.0 / 25.4 - rendered_width_pt) / 2.0,
        y_pt: bottom_mm * 72.0 / 25.4 + (max_height_pt - rendered_height_pt) / 2.0,
    }
}

fn build_evidence_pdf(
    case: &RectificationCase,
    products: &[ProductLine],
    evidence: &[EvidenceAsset],
) -> Result<Vec<u8>> {
    let mut cover_document = PdfDocument::new("Dossier de pruebas y valoración");
    let unicode_font = ParsedFont::from_bytes(ROBOTO_MEDIUM, 0, &mut Vec::new())
        .expect("La fuente Unicode incluida debe ser válida");
    let unicode_font_id = cover_document.add_font(&unicode_font);
    let mut cover = Vec::new();
    page_header(
        &mut cover,
        "DOSSIER DE PRUEBAS",
        &case.tracking_number,
        &unicode_font_id,
    );
    unicode_text(
        &mut cover,
        22.0,
        247.0,
        18.0,
        &unicode_font_id,
        &case.display_name,
    );
    unicode_text(
        &mut cover,
        22.0,
        236.0,
        10.0,
        &unicode_font_id,
        &format!("Guía internacional: {}", case.tracking_number),
    );
    unicode_text(
        &mut cover,
        22.0,
        228.0,
        9.0,
        &unicode_font_id,
        &format!(
            "Productos valorados: {}    Evidencias: {}",
            products.len(),
            evidence.len()
        ),
    );
    unicode_text(
        &mut cover,
        22.0,
        213.0,
        11.0,
        &unicode_font_id,
        "Resumen de valoración",
    );
    let visible_products: Vec<ProductLine> = products.iter().take(8).cloned().collect();
    product_table(&mut cover, &visible_products, 202.0, &unicode_font_id);
    unicode_text(
        &mut cover,
        22.0,
        82.0,
        11.0,
        &unicode_font_id,
        "Índice de evidencias",
    );
    let mut index_y = 73.0;
    for (index, asset) in evidence.iter().take(7).enumerate() {
        unicode_text(
            &mut cover,
            24.0,
            index_y,
            8.0,
            &unicode_font_id,
            &format!("{:02}. {}", index + 1, asset.document.title),
        );
        index_y -= 6.5;
    }
    if evidence.len() > 7 {
        unicode_text(
            &mut cover,
            24.0,
            index_y,
            8.0,
            &unicode_font_id,
            &format!("y {} evidencias adicionales", evidence.len() - 7),
        );
    }
    let mut segments = vec![save_pdf(
        &mut cover_document,
        vec![PdfPage::new(Mm(210.0), Mm(297.0), cover)],
    )];

    for (index, asset) in evidence.iter().enumerate() {
        if asset.document.is_image() {
            let mut image_document = PdfDocument::new(&format!("Evidencia {:02}", index + 1));
            let image_font = ParsedFont::from_bytes(ROBOTO_MEDIUM, 0, &mut Vec::new())
                .expect("La fuente Unicode incluida debe ser válida");
            let image_font_id = image_document.add_font(&image_font);
            let mut ops = Vec::new();
            page_header(
                &mut ops,
                &format!("EVIDENCIA {:02}", index + 1),
                &case.tracking_number,
                &image_font_id,
            );
            unicode_text(
                &mut ops,
                20.0,
                250.0,
                13.0,
                &image_font_id,
                &asset.document.title,
            );
            let image =
                RawImage::decode_from_bytes(&asset.bytes, &mut Vec::new()).map_err(|error| {
                    anyhow::anyhow!(
                        "No se pudo decodificar {}: {error}",
                        asset.document.original_filename
                    )
                })?;
            let placement =
                fit_image_to_printable_area(image.width, image.height, 300.0, 180.0, 220.0, 20.0);
            let image_id = image_document.add_image(&image);
            ops.push(Op::UseXobject {
                id: image_id,
                transform: XObjectTransform {
                    translate_x: Some(Pt(placement.x_pt)),
                    translate_y: Some(Pt(placement.y_pt)),
                    scale_x: Some(placement.scale),
                    scale_y: Some(placement.scale),
                    dpi: Some(300.0),
                    ..Default::default()
                },
            });
            segments.push(save_pdf(
                &mut image_document,
                vec![PdfPage::new(Mm(210.0), Mm(297.0), ops)],
            ));
        } else {
            LoDocument::load_mem(&asset.bytes).with_context(|| {
                format!(
                    "No se pudo incorporar el PDF original {}",
                    asset.document.original_filename
                )
            })?;
            segments.push(asset.bytes.clone());
        }
    }
    let segment_refs = segments.iter().map(Vec::as_slice).collect::<Vec<_>>();
    merge_pdf_documents(&segment_refs)
}

fn merge_pdf_documents(documents: &[&[u8]]) -> Result<Vec<u8>> {
    let mut output = LoDocument::with_version("1.5");
    let mut page_ids = Vec::new();
    let mut next_id = 1;

    for bytes in documents {
        let mut source = LoDocument::load_mem(bytes).context("No se pudo abrir un PDF generado")?;
        source.renumber_objects_with(next_id);
        next_id = source.max_id + 1;
        page_ids.extend(source.get_pages().into_values());

        for (object_id, object) in source.objects {
            if !matches!(object.type_name(), Ok(b"Catalog") | Ok(b"Pages")) {
                output.objects.insert(object_id, object);
            }
        }
        output.max_id = output.max_id.max(source.max_id);
    }

    let pages_id = output.new_object_id();
    for page_id in &page_ids {
        output
            .get_object_mut(*page_id)?
            .as_dict_mut()?
            .set("Parent", pages_id);
    }
    output.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Count" => page_ids.len() as i64,
            "Kids" => page_ids.iter().copied().map(Object::Reference).collect::<Vec<_>>(),
        }),
    );
    let catalog_id = output.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    output.trailer.set("Root", catalog_id);
    output.compress();
    let mut bytes = Vec::new();
    output.save_to(&mut bytes)?;
    Ok(bytes)
}

fn build_request_docx(
    case: &RectificationCase,
    applicant: &ApplicantDetails,
    products: &[ProductLine],
) -> Result<Vec<u8>> {
    let folio = case
        .customs_form_number
        .as_deref()
        .unwrap_or("No capturado");
    let presumptive = parse_mxn(&applicant.presumptive_value_mxn).unwrap_or(Decimal::ZERO);
    let assessment = postal_assessment(products, &applicant.usd_rate);
    let difference = (presumptive - assessment.total_mxn).max(Decimal::ZERO);
    let overvaluation = calculate_customs_overvaluation(presumptive, assessment.total_mxn);
    let merchandise = products
        .iter()
        .map(|product| {
            let unit = if product.quantity == 1 {
                "pieza"
            } else {
                "piezas"
            };
            format!("{} ({} {unit})", product.name, product.quantity)
        })
        .collect::<Vec<_>>()
        .join(", ");
    let original_values = products
        .iter()
        .map(|product| format!("{:.2} {}", product.total_original, product.currency))
        .collect::<Vec<_>>()
        .join("; ");
    let tax_conclusion = match assessment.treatment {
        PostalTaxTreatment::Exempt => format!(
            "Tomando como referencia el tipo de cambio documentado, el valor acreditado equivale a ${:.2} USD y se ubica por debajo del umbral de 50 USD. Por tal motivo, respetuosamente solicito que se verifique si resulta aplicable el supuesto previsto en la regla citada y, en su caso, se ajusten las contribuciones determinadas.",
            assessment.usd_equivalent
        ),
        PostalTaxTreatment::GlobalRate19 => format!(
            "En este caso, el valor acreditado de ${:.2} USD supera 50 USD y no excede 1,000 USD. El cálculo correcto bajo la tasa global es: ${:.2} M.N. x 19% = ${:.2} M.N.",
            assessment.usd_equivalent,
            assessment.total_mxn,
            assessment.tax_mxn.unwrap_or(Decimal::ZERO)
        ),
        PostalTaxTreatment::OutsideSimplifiedProcedure => format!(
            "El valor acreditado de ${:.2} USD excede el límite de 1,000 USD del procedimiento postal simplificado. Por ello no se presenta el 19% como importe definitivo y se solicita aplicar el procedimiento legal que corresponda sobre el valor real acreditado.",
            assessment.usd_equivalent
        ),
    };
    let payment_position = match assessment.tax_mxn {
        Some(amount) if amount > Decimal::ZERO => format!(
            "No me niego al pago de las contribuciones legalmente procedentes y manifiesto mi disposición a cubrir ${amount:.2} M.N.; solicito únicamente que el cobro se calcule sobre el valor real comprobado y no sobre una valuación excesiva."
        ),
        _ => "Manifiesto mi disposición a cumplir las obligaciones fiscales que legalmente correspondan y solicito respetuosamente que cualquier contribución se determine de manera fundada sobre el valor real comprobado.".to_owned(),
    };
    let third_petition = match assessment.treatment {
        PostalTaxTreatment::Exempt => "Se verifique la procedencia del tratamiento previsto para envíos de hasta 50 USD y, si se satisfacen los demás requisitos aplicables, se ajusten las contribuciones determinadas.".to_owned(),
        PostalTaxTreatment::GlobalRate19 => format!(
            "Se determine la tasa global del 19% sobre el valor real, por un importe de ${:.2} M.N., sujeto a confirmar que no exista una tasa específica aplicable.",
            assessment.tax_mxn.unwrap_or(Decimal::ZERO)
        ),
        PostalTaxTreatment::OutsideSimplifiedProcedure => "Se determine el procedimiento y las contribuciones legalmente aplicables sobre el valor real acreditado, sin emplear la tasa simplificada como importe definitivo.".to_owned(),
    };
    let fourth_petition = match assessment.treatment {
        PostalTaxTreatment::Exempt => "Con base en el resultado de esa revisión, se emita la rectificación del Formulario Postal que en derecho corresponda.".to_owned(),
        _ => match assessment.tax_mxn {
            Some(amount) => format!(
                "Se emita una Rectificación de Formulario Postal con importe de ${amount:.2} M.N. por los conceptos aquí calculados."
            ),
            None => "Se emita la rectificación correspondiente con el importe debidamente fundado y calculado.".to_owned(),
        },
    };

    let mut body = String::new();
    body.push_str(&word_subject());
    for line in [
        "C. ADMINISTRADOR DE LA ADUANA",
        "ATO CIUDAD DE MÉXICO",
        "OFICINA DE INTERCAMBIO POSTAL",
        "P R E S E N T E",
    ] {
        body.push_str(&word_paragraph(line, "Authority", false));
    }
    body.push_str(&word_paragraph(
        &format!(
            "Yo, {}, señalando como medios de contacto el correo electrónico {} y el teléfono {}, y como domicilio {}, comparezco respetuosamente para exponer:",
            applicant.full_name.trim(),
            applicant.email.trim(),
            value_or_default(&applicant.phone, "no proporcionado"),
            value_or_default(&applicant.address, "no proporcionado")
        ),
        "Normal",
        true,
    ));
    body.push_str(&word_paragraph("I. OBJETO", "Heading1", false));
    body.push_str(&word_paragraph(
        &format!(
            "Por medio del presente escrito, solicito la rectificación de la valoración aduanera efectuada al envío identificado con número de guía {} y boleta aduanal {}, toda vez que se determinó un valor presuntivo de ${:.2} M.N., el cual no corresponde al precio realmente pagado por la mercancía.",
            case.tracking_number, folio, presumptive
        ),
        "Normal",
        true,
    ));
    body.push_str(&word_paragraph("II. HECHOS Y CÁLCULO", "Heading1", false));
    let facts = [
        format!("La mercancía consiste en {}.", merchandise),
        format!(
            "El valor real de transacción acreditado es de {original_values}, equivalente a ${:.2} M.N. conforme a las tasas documentadas en el dossier.",
            assessment.total_mxn
        ),
        format!(
            "Para evaluar el umbral postal, el valor real equivale a ${:.2} USD usando 1 USD = {:.4} MXN, tasa de {} con fecha {}. Esta referencia debe confirmarse contra el tipo de cambio aduanero aplicable.",
            assessment.usd_equivalent,
            applicant.usd_rate.rate_to_mxn,
            applicant.usd_rate.source_name,
            applicant.usd_rate.rate_date
        ),
        overvaluation.map_or_else(
            || format!(
                "La boleta consigna ${:.2} M.N.; no se identifica un exceso positivo frente al valor real acreditado de ${:.2} M.N., por lo que se solicita revisar la discrepancia con base en la evidencia aportada.",
                presumptive, assessment.total_mxn
            ),
            |comparison| format!(
                "La boleta consigna ${:.2} M.N., es decir, una valuación ${:.2} M.N. superior al valor real acreditado, equivalente a un exceso de {:.2}%. Por tal motivo, respetuosamente solicito que esta diferencia sea revisada y, de estimarse procedente, se ajuste la valoración con base en la evidencia aportada.",
                presumptive,
                difference,
                comparison.percentage_above_real_value
            ),
        ),
    ];
    for fact in facts {
        body.push_str(&word_numbered_paragraph(&fact, 1));
    }
    body.push_str(&word_paragraph("III. FUNDAMENTO LEGAL", "Heading1", false));
    for paragraph in [
        "Conforme a los artículos 64 y 65 de la Ley Aduanera, la valoración debe partir del valor de transacción acreditado y no de una estimación presuntiva desproporcionada o ajena al precio efectivamente pagado.".to_owned(),
        "La RGCE 2026, regla 3.7.2, fracción I, dispone para envíos por vía postal el despacho sin pago de IGI, IVA ni DTA cuando el valor en aduana es igual o menor a 50 USD y la mercancía no está sujeta a regulaciones o restricciones no arancelarias.".to_owned(),
        "La fracción II de la misma regla permite, para valores mayores a 50 USD y hasta 1,000 USD, utilizar el Formulario Postal D1 aplicando una tasa global del 19%, salvo mercancías sujetas a tasas específicas u otros supuestos legales.".to_owned(),
        tax_conclusion,
        payment_position,
    ] {
        body.push_str(&word_paragraph(&paragraph, "Normal", true));
    }
    body.push_str(&word_paragraph(
        "IV. NATURALEZA NO COMERCIAL",
        "Heading1",
        false,
    ));
    body.push_str(&word_paragraph(
        value_or_default(
            &applicant.non_commercial_statement,
            "La mercancía corresponde a artículos en cantidad razonable para uso personal. No existe habitualidad, volumen comercial ni finalidad lucrativa que permita presumir actividad empresarial.",
        ),
        "Normal",
        true,
    ));
    if !applicant.request_notes.trim().is_empty() {
        body.push_str(&word_paragraph(
            &format!("Hechos adicionales: {}", applicant.request_notes.trim()),
            "Normal",
            true,
        ));
    }
    body.push_str(&word_paragraph("V. PETICIÓN", "Heading1", false));
    for petition in [
        format!(
            "Se revise la valoración presuntiva de ${:.2} M.N. y, de resultar procedente, se rectifique con base en la documentación aportada.",
            presumptive
        ),
        format!(
            "Se tenga por acreditado, previa revisión de las pruebas aportadas, el valor real de ${:.2} M.N.",
            assessment.total_mxn
        ),
        third_petition,
        fourth_petition,
    ] {
        body.push_str(&word_numbered_paragraph(&petition, 2));
    }
    body.push_str(&word_paragraph(
        &format!(
            "{}, {}, C.P. {}, a {}.",
            applicant.city.trim(),
            applicant.state.trim(),
            applicant.postal_code.trim(),
            applicant.issuance_date.trim()
        ),
        "Signature",
        false,
    ));
    body.push_str(&word_paragraph(
        applicant.full_name.trim(),
        "Signature",
        false,
    ));
    body.push_str(&word_paragraph(
        "Firma: ______________________________",
        "Signature",
        false,
    ));

    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>{body}<w:sectPr><w:headerReference w:type="default" r:id="rId3"/><w:footerReference w:type="default" r:id="rId4"/><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="708" w:footer="708" w:gutter="0"/></w:sectPr></w:body></w:document>"#
    );
    let generated_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (path, content) in [
        ("[Content_Types].xml", docx_content_types()),
        ("_rels/.rels", docx_root_relationships()),
        ("docProps/core.xml", docx_core_properties(&generated_at)),
        ("docProps/app.xml", docx_app_properties()),
        ("word/document.xml", document_xml),
        ("word/styles.xml", docx_styles()),
        ("word/numbering.xml", docx_numbering()),
        ("word/header1.xml", docx_header(&case.tracking_number)),
        ("word/footer1.xml", docx_footer()),
        (
            "word/_rels/document.xml.rels",
            docx_document_relationships(),
        ),
    ] {
        writer.start_file(path, options)?;
        writer.write_all(content.as_bytes())?;
    }
    Ok(writer.finish()?.into_inner())
}

fn word_paragraph(value: &str, style: &str, justify: bool) -> String {
    let alignment = if justify {
        r#"<w:jc w:val="both"/>"#
    } else {
        ""
    };
    format!(
        r#"<w:p><w:pPr><w:pStyle w:val="{}"/>{alignment}</w:pPr><w:r><w:t xml:space="preserve">{}</w:t></w:r></w:p>"#,
        xml_escape(style),
        xml_escape(value)
    )
}

fn word_subject() -> String {
    r#"<w:p><w:pPr><w:pStyle w:val="Subject"/></w:pPr><w:r><w:t>ASUNTO: Solicitud de rectificación de valoración aduanera por determinación</w:t><w:br/><w:t>presuntiva excesiva.</w:t></w:r></w:p>"#.to_owned()
}

fn word_numbered_paragraph(value: &str, number_id: u8) -> String {
    format!(
        r#"<w:p><w:pPr><w:pStyle w:val="ListNumber"/><w:numPr><w:ilvl w:val="0"/><w:numId w:val="{number_id}"/></w:numPr><w:jc w:val="both"/></w:pPr><w:r><w:t xml:space="preserve">{}</w:t></w:r></w:p>"#,
        xml_escape(value)
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn docx_content_types() -> String {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/><Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/><Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/></Types>"#.to_owned()
}

fn docx_root_relationships() -> String {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/></Relationships>"#.to_owned()
}

fn docx_document_relationships() -> String {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/><Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/></Relationships>"#.to_owned()
}

fn docx_core_properties(generated_at: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:dcmitype="http://purl.org/dc/dcmitype/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><dc:title>Solicitud de rectificación de valoración aduanera</dc:title><dc:creator>Mi Rectificación MX</dc:creator><cp:lastModifiedBy>Mi Rectificación MX</cp:lastModifiedBy><dcterms:created xsi:type="dcterms:W3CDTF">{generated_at}</dcterms:created><dcterms:modified xsi:type="dcterms:W3CDTF">{generated_at}</dcterms:modified></cp:coreProperties>"#
    )
}

fn docx_app_properties() -> String {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><Application>Mi Rectificación MX</Application><AppVersion>0.1</AppVersion></Properties>"#.to_owned()
}

fn docx_styles() -> String {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:cs="Calibri"/><w:sz w:val="22"/><w:szCs w:val="22"/><w:lang w:val="es-MX"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after="120" w:line="264" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:qFormat/><w:pPr><w:spacing w:before="0" w:after="120" w:line="264" w:lineRule="auto"/></w:pPr><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri"/><w:sz w:val="22"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Subject"><w:name w:val="Asunto"/><w:basedOn w:val="Normal"/><w:pPr><w:spacing w:after="160"/></w:pPr><w:rPr><w:b/><w:sz w:val="22"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Authority"><w:name w:val="Autoridad"/><w:basedOn w:val="Normal"/><w:pPr><w:spacing w:after="0"/></w:pPr><w:rPr><w:b/><w:sz w:val="22"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:qFormat/><w:keepNext/><w:pPr><w:spacing w:before="320" w:after="160"/><w:outlineLvl w:val="0"/></w:pPr><w:rPr><w:b/><w:color w:val="2E74B5"/><w:sz w:val="32"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="ListNumber"><w:name w:val="List Number"/><w:basedOn w:val="Normal"/><w:pPr><w:spacing w:after="160" w:line="280" w:lineRule="auto"/></w:pPr></w:style><w:style w:type="paragraph" w:styleId="Signature"><w:name w:val="Firma"/><w:basedOn w:val="Normal"/><w:pPr><w:spacing w:before="80" w:after="80"/></w:pPr></w:style></w:styles>"#.to_owned()
}

fn docx_numbering() -> String {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:multiLevelType w:val="singleLevel"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:lvlJc w:val="left"/><w:pPr><w:tabs><w:tab w:val="num" w:pos="720"/></w:tabs><w:ind w:left="720" w:hanging="360"/><w:spacing w:after="160" w:line="280" w:lineRule="auto"/></w:pPr><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri"/><w:sz w:val="22"/></w:rPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#.to_owned()
}

fn docx_header(tracking_number: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:pPr><w:jc w:val="right"/><w:spacing w:after="0"/></w:pPr><w:r><w:rPr><w:color w:val="777777"/><w:sz w:val="18"/></w:rPr><w:t>Solicitud de rectificación | {}</w:t></w:r></w:p></w:hdr>"#,
        xml_escape(tracking_number)
    )
}

fn docx_footer() -> String {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:ftr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:pPr><w:jc w:val="right"/><w:spacing w:before="0" w:after="0"/></w:pPr><w:r><w:rPr><w:color w:val="777777"/><w:sz w:val="18"/></w:rPr><w:t>Página </w:t></w:r><w:fldSimple w:instr="PAGE"><w:r><w:rPr><w:color w:val="777777"/><w:sz w:val="18"/></w:rPr><w:t>1</w:t></w:r></w:fldSimple></w:p></w:ftr>"#.to_owned()
}

pub fn compose_email(
    case: &RectificationCase,
    applicant: &ApplicantDetails,
    products: &[ProductLine],
) -> EmailContent {
    let assessment = postal_assessment(products, &applicant.usd_rate);
    let calculation = match assessment.treatment {
        PostalTaxTreatment::Exempt => format!(
            "El valor acreditado equivale a {:.2} USD y se solicita el despacho sin IGI, IVA ni DTA conforme a la regla 3.7.2, fracción I, sujeto al cumplimiento de sus demás requisitos.",
            assessment.usd_equivalent
        ),
        PostalTaxTreatment::GlobalRate19 => format!(
            "El valor acreditado equivale a {:.2} USD. La tasa global del 19% sobre ${:.2} MXN arroja un importe de ${:.2} MXN, que manifiesto estar dispuesto a cubrir una vez rectificada la valoración.",
            assessment.usd_equivalent,
            assessment.total_mxn,
            assessment.tax_mxn.unwrap_or(Decimal::ZERO)
        ),
        PostalTaxTreatment::OutsideSimplifiedProcedure => format!(
            "El valor acreditado equivale a {:.2} USD y supera el límite del procedimiento postal simplificado; solicito que se indique y aplique el procedimiento correcto sobre el valor real comprobado.",
            assessment.usd_equivalent
        ),
    };
    EmailContent {
        recipient: applicant.authority_email.trim().to_owned(),
        sender: applicant.email.trim().to_owned(),
        subject: format!("Solicitud de rectificación - guía {}", case.tracking_number),
        body: format!(
            "A la atención de {}:\n\nAdjunto la solicitud de rectificación y el dossier de pruebas correspondientes a la guía {}. {} No me niego al pago de las contribuciones legalmente procedentes; solicito que se calculen sobre el valor real acreditado. Agradezco la confirmación de recepción y las indicaciones para continuar el trámite.\n\nAtentamente,\n{}\n{}\n{}",
            applicant.authority_name,
            case.tracking_number,
            calculation,
            applicant.full_name,
            applicant.email,
            applicant.phone
        ),
    }
}

pub fn write_email_draft(
    path: &Path,
    content: &EmailContent,
    request_pdf: &Path,
    evidence_pdf: &Path,
) -> Result<()> {
    validate_email_content(content)?;
    let request_bytes = fs::read(request_pdf)
        .with_context(|| format!("No se pudo leer {}", request_pdf.display()))?;
    let evidence_bytes = fs::read(evidence_pdf)
        .with_context(|| format!("No se pudo leer {}", evidence_pdf.display()))?;
    let message = build_email_message(
        content,
        &[
            ("01_solicitud_rectificacion.pdf", request_bytes.as_slice()),
            ("02_dossier_pruebas.pdf", evidence_bytes.as_slice()),
        ],
    );
    write_atomic(path, message.as_bytes())
}

fn validate_email_content(content: &EmailContent) -> Result<()> {
    for (label, value) in [
        ("destinatario", content.recipient.as_str()),
        ("remitente", content.sender.as_str()),
    ] {
        if value.trim().is_empty() || !value.contains('@') || value.contains(['\r', '\n']) {
            bail!("El {label} del correo no es válido");
        }
    }
    if content.subject.trim().is_empty() || content.subject.contains(['\r', '\n']) {
        bail!("El asunto del correo no es válido");
    }
    if content.body.trim().is_empty() {
        bail!("El cuerpo del correo no puede quedar vacío");
    }
    Ok(())
}

fn build_email_message(content: &EmailContent, attachments: &[(&str, &[u8])]) -> String {
    const BOUNDARY: &str = "----MiRectificacionMX-Adjuntos-202608";
    let mut message = format!(
        "To: {}\r\nFrom: {}\r\nSubject: {}\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"{}\"\r\n\r\n--{}\r\nContent-Type: text/plain; charset=UTF-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{}\r\n",
        content.recipient.trim(),
        content.sender.trim(),
        content.subject.trim(),
        BOUNDARY,
        BOUNDARY,
        content.body.trim().replace('\n', "\r\n")
    );
    for (filename, bytes) in attachments {
        let encoded = STANDARD.encode(bytes);
        let wrapped = encoded
            .as_bytes()
            .chunks(76)
            .map(|chunk| String::from_utf8_lossy(chunk))
            .collect::<Vec<_>>()
            .join("\r\n");
        message.push_str(&format!(
            "--{BOUNDARY}\r\nContent-Type: application/pdf; name=\"{filename}\"\r\nContent-Transfer-Encoding: base64\r\nContent-Disposition: attachment; filename=\"{filename}\"\r\n\r\n{wrapped}\r\n"
        ));
    }
    message.push_str(&format!("--{BOUNDARY}--\r\n"));
    message
}

fn product_table(ops: &mut Vec<Op>, products: &[ProductLine], start_y: f32, font: &FontId) {
    unicode_text(ops, 22.0, start_y, 8.0, font, "Producto / cantidad");
    unicode_text(ops, 95.0, start_y, 8.0, font, "Valor original");
    unicode_text(ops, 133.0, start_y, 8.0, font, "Tasa / fecha");
    unicode_text(ops, 172.0, start_y, 8.0, font, "MXN");
    let mut y = start_y - 8.0;
    if products.is_empty() {
        unicode_text(ops, 22.0, y, 8.0, font, "Sin productos capturados");
        return;
    }
    for product in products {
        unicode_text(
            ops,
            22.0,
            y,
            7.5,
            font,
            &truncate(&format!("{} (x{})", product.name, product.quantity), 37),
        );
        unicode_text(
            ops,
            95.0,
            y,
            7.5,
            font,
            &format!("{:.2} {}", product.total_original, product.currency),
        );
        unicode_text(
            ops,
            133.0,
            y,
            7.0,
            font,
            &format!("{} / {}", product.rate.rate_to_mxn, product.rate.rate_date),
        );
        unicode_text(
            ops,
            172.0,
            y,
            7.5,
            font,
            &format!("${:.2}", product.total_mxn),
        );
        y -= 10.0;
    }
}

fn page_header(ops: &mut Vec<Op>, title: &str, tracking: &str, font: &FontId) {
    unicode_text(ops, 20.0, 270.0, 15.0, font, title);
    ascii_text(ops, 160.0, 281.0, 8.0, BuiltinFont::Courier, tracking);
}

fn page_footer(ops: &mut Vec<Op>, page: usize, total: usize) {
    ascii_text(
        ops,
        178.0,
        14.0,
        7.0,
        BuiltinFont::Helvetica,
        &format!("{page}/{total}"),
    );
}

fn ascii_text(ops: &mut Vec<Op>, x: f32, y: f32, size: f32, font: BuiltinFont, value: &str) {
    let value = ascii_safe_text(value);
    ops.extend([
        Op::StartTextSection,
        Op::SetTextCursor {
            pos: Point::new(Mm(x), Mm(y)),
        },
        Op::SetFontSizeBuiltinFont {
            size: Pt(size),
            font,
        },
        Op::SetFillColor {
            col: Color::Rgb(Rgb::new(0.13, 0.15, 0.13, None)),
        },
        Op::WriteTextBuiltinFont {
            items: vec![TextItem::Text(value)],
            font,
        },
        Op::EndTextSection,
    ]);
}

fn save_pdf(document: &mut PdfDocument, pages: Vec<PdfPage>) -> Vec<u8> {
    document
        .with_pages(pages)
        .save(&PdfSaveOptions::default(), &mut Vec::new())
}

fn create_zip(path: &Path, generated: &[(&Path, &[u8])], evidence: &[EvidenceAsset]) -> Result<()> {
    let temporary = path.with_extension("zip.tmp");
    let file = File::create(&temporary)?;
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (source, bytes) in generated {
        writer.start_file(file_name(source)?, options)?;
        writer.write_all(bytes)?;
    }
    for (index, asset) in evidence.iter().enumerate() {
        writer.start_file(
            format!(
                "evidencias/{:02}_{}",
                index + 1,
                sanitize_filename(&asset.document.original_filename)
            ),
            options,
        )?;
        writer.write_all(&asset.bytes)?;
    }
    writer.finish()?.sync_all()?;
    replace_file(&temporary, path)?;
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes)?;
    replace_file(&temporary, path)?;
    Ok(())
}

fn replace_file(temporary: &Path, destination: &Path) -> Result<()> {
    #[cfg(target_os = "windows")]
    if destination.exists() {
        fs::remove_file(destination)?;
    }
    fs::rename(temporary, destination)?;
    Ok(())
}
fn file_name(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .context("Nombre de archivo inválido")
}
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn sanitize_filename(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}
fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_owned()
    } else {
        format!(
            "{}...",
            value
                .chars()
                .take(max.saturating_sub(3))
                .collect::<String>()
        )
    }
}
fn value_or_dash(value: &str) -> &str {
    if value.trim().is_empty() {
        "No capturado"
    } else {
        value.trim()
    }
}

fn value_or_default<'a>(value: &'a str, default: &'a str) -> &'a str {
    if value.trim().is_empty() {
        default
    } else {
        value.trim()
    }
}

fn parse_mxn(value: &str) -> Result<Decimal> {
    let normalized = value
        .trim()
        .replace(['$', ','], "")
        .replace("M.N.", "")
        .replace("MXN", "")
        .trim()
        .to_owned();
    Decimal::from_str(&normalized)
        .context("El valor presuntivo no tiene un formato numérico válido")
}

fn ascii_safe_text(value: &str) -> String {
    value
        .chars()
        .map(|character| if character.is_ascii() { character } else { '?' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use mi_rectificacion_domain::{EvidenceKind, ProductDraft};
    use printpdf::PdfParseOptions;
    use uuid::Uuid;

    fn email_attachment(email: &str, filename: &str) -> Vec<u8> {
        let marker = format!("Content-Disposition: attachment; filename=\"{filename}\"");
        let attachment = email
            .split(&marker)
            .nth(1)
            .unwrap_or_else(|| panic!("No se encontró el adjunto {filename}"));
        let encoded = attachment
            .split("\r\n\r\n")
            .nth(1)
            .unwrap_or_else(|| panic!("El adjunto {filename} no contiene datos"))
            .split("\r\n------MiRectificacionMX-Adjuntos-202608")
            .next()
            .unwrap()
            .replace("\r\n", "");
        STANDARD
            .decode(encoded)
            .unwrap_or_else(|error| panic!("El adjunto {filename} no es base64 válido: {error}"))
    }

    fn test_rate(currency: &str, rate: Decimal) -> ExchangeRateSnapshot {
        ExchangeRateSnapshot::automatic(
            currency,
            NaiveDate::from_ymd_opt(2026, 8, 14).unwrap(),
            rate,
            "Fuente de prueba",
            "https://example.test/rate",
            Utc::now(),
        )
        .unwrap()
    }

    #[test]
    fn unicode_text_keeps_spanish_characters_unchanged() {
        let expected = "Rectificación, valoración, guía, México, niño, pingüino, ¿sí? ¡Sí!";
        let mut operations = Vec::new();
        unicode_text(
            &mut operations,
            10.0,
            10.0,
            10.0,
            &FontId("test-font".to_owned()),
            expected,
        );
        assert!(operations.iter().any(|operation| matches!(
            operation,
            Op::WriteText { items, .. }
                if items == &vec![TextItem::Text(expected.to_owned())]
        )));
    }

    #[test]
    fn horizontal_evidence_uses_the_full_printable_width() {
        let placement = fit_image_to_printable_area(1_200, 300, 300.0, 180.0, 205.0, 20.0);
        let expected_width = 180.0 * 72.0 / 25.4;
        let rendered_width = 1_200.0 * 72.0 / 300.0 * placement.scale;
        let rendered_height = 300.0 * 72.0 / 300.0 * placement.scale;

        assert!(placement.scale > 1.0);
        assert!((rendered_width - expected_width).abs() < 0.1);
        assert!(rendered_height < 205.0 * 72.0 / 25.4);
    }

    #[test]
    fn vertical_evidence_uses_the_full_printable_height() {
        let placement = fit_image_to_printable_area(900, 1_800, 300.0, 180.0, 205.0, 20.0);
        let expected_height = 205.0 * 72.0 / 25.4;
        let rendered_width = 900.0 * 72.0 / 300.0 * placement.scale;
        let rendered_height = 1_800.0 * 72.0 / 300.0 * placement.scale;

        assert!(placement.scale > 1.0);
        assert!((rendered_height - expected_height).abs() < 0.1);
        assert!(rendered_width < 180.0 * 72.0 / 25.4);
    }

    fn test_product(case: &RectificationCase, total_mxn: Decimal) -> ProductLine {
        let draft = ProductDraft::new(
            "Producto de prueba",
            None,
            1,
            total_mxn,
            Decimal::ZERO,
            Decimal::ZERO,
            Decimal::ZERO,
            "MXN",
        )
        .unwrap();
        ProductLine::new(case.id, draft, test_rate("MXN", Decimal::ONE)).unwrap()
    }

    fn test_invoice_pdf() -> Vec<u8> {
        let mut document = PdfDocument::new("Factura PDF de prueba");
        let mut ops = Vec::new();
        ascii_text(
            &mut ops,
            7.0,
            289.0,
            10.0,
            BuiltinFont::Helvetica,
            "ESQUINA SUPERIOR IZQUIERDA",
        );
        ascii_text(
            &mut ops,
            132.0,
            289.0,
            10.0,
            BuiltinFont::Helvetica,
            "SUPERIOR DERECHA",
        );
        ascii_text(
            &mut ops,
            7.0,
            7.0,
            10.0,
            BuiltinFont::Helvetica,
            "ESQUINA INFERIOR IZQUIERDA",
        );
        ascii_text(
            &mut ops,
            132.0,
            7.0,
            10.0,
            BuiltinFont::Helvetica,
            "INFERIOR DERECHA",
        );
        save_pdf(&mut document, vec![PdfPage::new(Mm(210.0), Mm(297.0), ops)])
    }

    #[test]
    fn generates_reopenable_bundle_with_manifest_and_zip() {
        let qa_root = std::env::var_os("MRMX_QA_OUTPUT").map(PathBuf::from);
        let root = qa_root.clone().unwrap_or_else(|| {
            std::env::temp_dir().join(format!("mrmx-documents-{}", Uuid::new_v4()))
        });
        let case = RectificationCase::new(
            "RR123456789MX",
            Some("BOLETA-TEST".to_owned()),
            Some("Caso documental".to_owned()),
        )
        .unwrap();
        let applicant = ApplicantDetails {
            full_name: "Persona de prueba".to_owned(),
            email: "persona@example.com".to_owned(),
            phone: String::new(),
            address: String::new(),
            authority_name: "Autoridad de prueba".to_owned(),
            authority_email: "autoridad@example.gob.mx".to_owned(),
            presumptive_value_mxn: "6055.87".to_owned(),
            city: "Puebla".to_owned(),
            state: "Puebla".to_owned(),
            postal_code: "72000".to_owned(),
            issuance_date: "14 de agosto de 2026".to_owned(),
            non_commercial_statement: String::new(),
            request_notes: "Revisión sintética".to_owned(),
            usd_rate: test_rate("USD", Decimal::new(20, 0)),
        };
        let products = [test_product(&case, Decimal::new(88711, 2))];
        let evidence_bytes =
            include_bytes!("../../../assets/mi-rectificacion-mx-logo.png").to_vec();
        let invoice_pdf = test_invoice_pdf();
        let evidence = vec![
            EvidenceAsset {
                document: EvidenceDocument {
                    id: Uuid::new_v4(),
                    case_id: case.id,
                    kind: EvidenceKind::Transaction,
                    title: "Captura de transacción de prueba".to_owned(),
                    original_filename: "captura-transaccion.png".to_owned(),
                    content_type: "image/png".to_owned(),
                    size_bytes: evidence_bytes.len() as u64,
                    sha256: sha256_hex(&evidence_bytes),
                    encrypted_relative_path: "fixture".to_owned(),
                    order_index: 0,
                    created_at: Utc::now(),
                },
                bytes: evidence_bytes.clone(),
            },
            EvidenceAsset {
                document: EvidenceDocument {
                    id: Uuid::new_v4(),
                    case_id: case.id,
                    kind: EvidenceKind::Product,
                    title: "Factura PDF completa".to_owned(),
                    original_filename: "factura-prueba.pdf".to_owned(),
                    content_type: "application/pdf".to_owned(),
                    size_bytes: invoice_pdf.len() as u64,
                    sha256: sha256_hex(&invoice_pdf),
                    encrypted_relative_path: "fixture-pdf".to_owned(),
                    order_index: 1,
                    created_at: Utc::now(),
                },
                bytes: invoice_pdf,
            },
        ];

        let bundle = generate_bundle(&case, &applicant, &products, &evidence, &root).unwrap();
        let regenerated = generate_bundle(&case, &applicant, &products, &evidence, &root).unwrap();
        assert_eq!(bundle.directory, regenerated.directory);
        let standalone_pdf = root.join("expediente-elegido.pdf");
        let standalone_docx = root.join("expediente-elegido.docx");
        export_print_ready_pdf(&case, &applicant, &products, &evidence, &standalone_pdf).unwrap();
        export_editable_docx(&case, &applicant, &products, &standalone_docx).unwrap();
        assert_eq!(
            PdfDocument::parse(
                &fs::read(&standalone_pdf).unwrap(),
                &PdfParseOptions::default(),
                &mut Vec::new()
            )
            .unwrap()
            .pages
            .len(),
            5
        );
        assert!(zip::ZipArchive::new(File::open(&standalone_docx).unwrap()).is_ok());
        let request = fs::read(&bundle.request_pdf).unwrap();
        let dossier = fs::read(&bundle.evidence_pdf).unwrap();
        let parsed_request =
            PdfDocument::parse(&request, &PdfParseOptions::default(), &mut Vec::new()).unwrap();
        assert_eq!(parsed_request.pages.len(), 2);
        let opening_lines = parsed_request.pages[0]
            .ops
            .iter()
            .filter_map(|operation| match operation {
                Op::WriteText { items, .. } => items.first(),
                _ => None,
            })
            .filter_map(|item| match item {
                TextItem::Text(value) => Some(value.as_str()),
                _ => None,
            })
            .take(6)
            .collect::<Vec<_>>();
        assert_eq!(
            opening_lines,
            vec![
                "ASUNTO: Solicitud de rectificación de valoración aduanera por determinación",
                "presuntiva excesiva.",
                "C. ADMINISTRADOR DE LA ADUANA",
                "ATO CIUDAD DE MÉXICO",
                "OFICINA DE INTERCAMBIO POSTAL",
                "P R E S E N T E",
            ]
        );
        let request_text = parsed_request
            .pages
            .iter()
            .flat_map(|page| page.ops.iter())
            .filter_map(|operation| match operation {
                Op::WriteText { items, .. } | Op::WriteTextBuiltinFont { items, .. } => {
                    items.first()
                }
                _ => None,
            })
            .filter_map(|item| match item {
                TextItem::Text(value) => Some(value.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(request_text.contains(
            "respetuosamente solicito que esta diferencia sea revisada y, de estimarse procedente"
        ));
        assert!(!request_text.contains("Esta diferencia debe rectificarse"));
        let parsed_dossier =
            PdfDocument::parse(&dossier, &PdfParseOptions::default(), &mut Vec::new()).unwrap();
        assert_eq!(parsed_dossier.pages.len(), 3);
        let visible_dossier_text = parsed_dossier
            .pages
            .iter()
            .flat_map(|page| page.ops.iter())
            .filter_map(|operation| match operation {
                Op::WriteText { items, .. } | Op::WriteTextBuiltinFont { items, .. } => {
                    items.first()
                }
                _ => None,
            })
            .filter_map(|item| match item {
                TextItem::Text(value) => Some(value.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(visible_dossier_text.contains("EVIDENCIA 01"));
        assert!(visible_dossier_text.contains("Captura de transacción de prueba"));
        assert!(visible_dossier_text.contains("Factura PDF completa"));
        assert!(visible_dossier_text.contains("ESQUINA SUPERIOR IZQUIERDA"));
        assert!(visible_dossier_text.contains("ESQUINA INFERIOR IZQUIERDA"));
        assert!(!visible_dossier_text.contains("captura-transaccion.png"));
        assert!(!visible_dossier_text.contains("factura-prueba.pdf"));
        assert!(!visible_dossier_text.contains("SHA-256"));
        assert!(!visible_dossier_text.contains("cifrad"));
        let source_invoice = LoDocument::load_mem(&evidence[1].bytes).unwrap();
        let merged_dossier = LoDocument::load_mem(&dossier).unwrap();
        let source_page = *source_invoice.get_pages().values().next().unwrap();
        let merged_pdf_page = *merged_dossier.get_pages().values().nth(2).unwrap();
        assert_eq!(
            merged_dossier.get_page_content(merged_pdf_page).unwrap(),
            source_invoice.get_page_content(source_page).unwrap()
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&bundle.manifest).unwrap()).unwrap();
        assert_eq!(manifest["template_version"], TEMPLATE_VERSION);
        assert_eq!(manifest["files"].as_array().unwrap().len(), 5);
        let print_ready = fs::read(&bundle.print_pdf).unwrap();
        assert_eq!(
            PdfDocument::parse(&print_ready, &PdfParseOptions::default(), &mut Vec::new())
                .unwrap()
                .pages
                .len(),
            5
        );
        let mut editable = zip::ZipArchive::new(File::open(&bundle.request_docx).unwrap()).unwrap();
        let mut editable_xml = String::new();
        std::io::Read::read_to_string(
            &mut editable.by_name("word/document.xml").unwrap(),
            &mut editable_xml,
        )
        .unwrap();
        assert!(editable_xml.contains("P R E S E N T E"));
        assert!(editable_xml.contains("determinación</w:t><w:br/><w:t>presuntiva excesiva."));
        assert!(editable_xml.contains("Persona de prueba"));
        assert!(editable_xml.contains("artículos 64 y 65 de la Ley Aduanera"));
        assert!(editable_xml.contains("respetuosamente solicito que se verifique"));
        assert!(editable_xml.contains("que en derecho corresponda"));
        assert!(!editable_xml.contains("importe correcto solicitado"));
        assert!(!editable_xml.contains("Se retiren los cargos"));
        assert!(!editable_xml.contains("importe de $0.00 M.N."));
        let comparison =
            calculate_customs_overvaluation(Decimal::new(605_587, 2), Decimal::new(88_711, 2))
                .unwrap();
        assert!(editable_xml.contains(&format!(
            "equivalente a un exceso de {:.2}%",
            comparison.percentage_above_real_value
        )));
        assert!(editable_xml.contains(
            "respetuosamente solicito que esta diferencia sea revisada y, de estimarse procedente"
        ));
        assert!(!editable_xml.contains("Esta diferencia debe rectificarse"));
        let mut archive = zip::ZipArchive::new(File::open(&bundle.zip).unwrap()).unwrap();
        assert!(archive.by_name("01_solicitud_rectificacion.pdf").is_ok());
        assert!(archive.by_name("02_dossier_pruebas.pdf").is_ok());
        assert!(
            archive
                .by_name("03_expediente_listo_para_imprimir.pdf")
                .is_ok()
        );
        assert!(
            archive
                .by_name("04_solicitud_rectificacion_editable.docx")
                .is_ok()
        );
        assert!(archive.by_name("05_correo_aduanas.eml").is_ok());
        assert!(archive.by_name("manifest.json").is_ok());
        let mut archived_evidence = Vec::new();
        std::io::Read::read_to_end(
            &mut archive
                .by_name("evidencias/01_captura-transaccion.png")
                .unwrap(),
            &mut archived_evidence,
        )
        .unwrap();
        assert_eq!(archived_evidence, evidence_bytes);
        let email = fs::read_to_string(&bundle.email_draft).unwrap();
        assert!(email.contains("Content-Type: multipart/mixed"));
        assert!(email.contains("filename=\"01_solicitud_rectificacion.pdf\""));
        assert!(email.contains("filename=\"02_dossier_pruebas.pdf\""));
        assert_eq!(
            email_attachment(&email, "01_solicitud_rectificacion.pdf"),
            request
        );
        assert_eq!(email_attachment(&email, "02_dossier_pruebas.pdf"), dossier);
        assert_eq!(bundle.email_content.recipient, applicant.authority_email);
        if qa_root.is_none() {
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn rejects_an_incomplete_recipient() {
        let applicant = ApplicantDetails {
            full_name: "Persona".to_owned(),
            email: "persona@example.com".to_owned(),
            phone: String::new(),
            address: String::new(),
            authority_name: "Autoridad".to_owned(),
            authority_email: String::new(),
            presumptive_value_mxn: "6055.87".to_owned(),
            city: "Puebla".to_owned(),
            state: "Puebla".to_owned(),
            postal_code: "72000".to_owned(),
            issuance_date: "14 de agosto de 2026".to_owned(),
            non_commercial_statement: String::new(),
            request_notes: String::new(),
            usd_rate: test_rate("USD", Decimal::new(20, 0)),
        };
        assert!(validate_applicant(&applicant).is_err());
    }

    #[test]
    fn selects_the_postal_treatment_from_the_usd_threshold() {
        let case = RectificationCase::new("RR123456789MX", None, None).unwrap();
        let usd_rate = test_rate("USD", Decimal::new(20, 0));

        let exempt = [test_product(&case, Decimal::new(1000, 0))];
        let taxed = [test_product(&case, Decimal::new(2000, 0))];
        let outside = [test_product(&case, Decimal::new(25000, 0))];

        let exempt_result = postal_assessment(&exempt, &usd_rate);
        assert_eq!(exempt_result.usd_equivalent, Decimal::new(50, 0));
        assert_eq!(exempt_result.treatment, PostalTaxTreatment::Exempt);
        assert_eq!(exempt_result.tax_mxn, Some(Decimal::ZERO));

        let taxed_result = postal_assessment(&taxed, &usd_rate);
        assert_eq!(taxed_result.treatment, PostalTaxTreatment::GlobalRate19);
        assert_eq!(taxed_result.tax_mxn, Some(Decimal::new(380, 0)));

        let outside_result = postal_assessment(&outside, &usd_rate);
        assert_eq!(
            outside_result.treatment,
            PostalTaxTreatment::OutsideSimplifiedProcedure
        );
        assert_eq!(outside_result.tax_mxn, None);
    }
}
