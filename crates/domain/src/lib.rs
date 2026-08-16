use chrono::{DateTime, Utc};
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use thiserror::Error;
use uuid::Uuid;

pub const MAX_EVIDENCE_SIZE_BYTES: u64 = 25 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustomsOvervaluation {
    pub difference_mxn: Decimal,
    pub percentage_above_real_value: Decimal,
}

pub fn calculate_customs_overvaluation(
    customs_value_mxn: Decimal,
    real_value_mxn: Decimal,
) -> Option<CustomsOvervaluation> {
    if customs_value_mxn <= real_value_mxn || real_value_mxn <= Decimal::ZERO {
        return None;
    }

    let difference_mxn = customs_value_mxn - real_value_mxn;
    let percentage_above_real_value = (difference_mxn / real_value_mxn * Decimal::ONE_HUNDRED)
        .round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero);

    Some(CustomsOvervaluation {
        difference_mxn,
        percentage_above_real_value,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseStatus {
    Draft,
    MissingEvidence,
    ReadyToGenerate,
    DocumentsGenerated,
    EmailPrepared,
    Sent,
    Resolved,
    Closed,
}

impl CaseStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::MissingEvidence => "missing_evidence",
            Self::ReadyToGenerate => "ready_to_generate",
            Self::DocumentsGenerated => "documents_generated",
            Self::EmailPrepared => "email_prepared",
            Self::Sent => "sent",
            Self::Resolved => "resolved",
            Self::Closed => "closed",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Draft => "Borrador",
            Self::MissingEvidence => "Faltan pruebas",
            Self::ReadyToGenerate => "Listo para generar",
            Self::DocumentsGenerated => "Documentos generados",
            Self::EmailPrepared => "Correo preparado",
            Self::Sent => "Enviado",
            Self::Resolved => "Resuelto",
            Self::Closed => "Cerrado",
        }
    }
}

impl fmt::Display for CaseStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for CaseStatus {
    type Err = DomainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "draft" => Ok(Self::Draft),
            "missing_evidence" => Ok(Self::MissingEvidence),
            "ready_to_generate" => Ok(Self::ReadyToGenerate),
            "documents_generated" => Ok(Self::DocumentsGenerated),
            "email_prepared" => Ok(Self::EmailPrepared),
            "sent" => Ok(Self::Sent),
            "resolved" => Ok(Self::Resolved),
            "closed" => Ok(Self::Closed),
            _ => Err(DomainError::UnknownCaseStatus(value.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RectificationCase {
    pub id: Uuid,
    pub display_name: String,
    pub tracking_number: String,
    pub customs_form_number: Option<String>,
    pub status: CaseStatus,
    pub has_unseen_updates: bool,
    pub archived_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ApplicantProfile {
    pub full_name: String,
    pub email: String,
    pub phone: String,
    pub address: String,
    pub city: String,
    pub state: String,
    pub postal_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailDraft {
    pub case_id: Uuid,
    pub recipient: String,
    pub sender: String,
    pub subject: String,
    pub body: String,
    pub request_pdf_path: String,
    pub evidence_pdf_path: String,
    pub eml_path: String,
    pub prepared_at: DateTime<Utc>,
    pub opened_at: Option<DateTime<Utc>>,
    pub sent_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    CustomsForm,
    Transaction,
    BankStatement,
    Product,
    Other,
}

impl EvidenceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CustomsForm => "customs_form",
            Self::Transaction => "transaction",
            Self::BankStatement => "bank_statement",
            Self::Product => "product",
            Self::Other => "other",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::CustomsForm => "Boleta aduanal",
            Self::Transaction => "Comprobante de transacción",
            Self::BankStatement => "Estado de cuenta",
            Self::Product => "Factura o producto",
            Self::Other => "Otro anexo",
        }
    }
}

impl FromStr for EvidenceKind {
    type Err = DomainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "customs_form" => Ok(Self::CustomsForm),
            "transaction" => Ok(Self::Transaction),
            "bank_statement" => Ok(Self::BankStatement),
            "product" => Ok(Self::Product),
            "other" => Ok(Self::Other),
            _ => Err(DomainError::UnknownEvidenceKind(value.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceDocument {
    pub id: Uuid,
    pub case_id: Uuid,
    pub kind: EvidenceKind,
    pub title: String,
    pub original_filename: String,
    pub content_type: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub encrypted_relative_path: String,
    pub order_index: i64,
    pub created_at: DateTime<Utc>,
}

impl EvidenceDocument {
    pub fn is_image(&self) -> bool {
        self.content_type.starts_with("image/")
    }

    pub fn size_label(&self) -> String {
        if self.size_bytes >= 1024 * 1024 {
            format!("{:.1} MB", self.size_bytes as f64 / (1024.0 * 1024.0))
        } else {
            format!("{:.1} KB", self.size_bytes as f64 / 1024.0)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: Uuid,
    pub case_id: Uuid,
    pub event_type: String,
    pub summary: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackingEventInput {
    pub occurred_at: Option<DateTime<Utc>>,
    pub description: String,
    pub location: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackingEvent {
    pub id: Uuid,
    pub case_id: Uuid,
    pub fingerprint: String,
    pub occurred_at: Option<DateTime<Utc>>,
    pub description: String,
    pub location: Option<String>,
    pub source: String,
    pub is_seen: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TrackingRefreshState {
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeRateSnapshot {
    pub currency: String,
    pub rate_date: chrono::NaiveDate,
    pub rate_to_mxn: Decimal,
    pub source_name: String,
    pub source_url: String,
    pub fetched_at: DateTime<Utc>,
    pub is_manual: bool,
    pub manual_reason: Option<String>,
}

impl ExchangeRateSnapshot {
    pub fn automatic(
        currency: impl Into<String>,
        rate_date: chrono::NaiveDate,
        rate_to_mxn: Decimal,
        source_name: impl Into<String>,
        source_url: impl Into<String>,
        fetched_at: DateTime<Utc>,
    ) -> Result<Self, DomainError> {
        Self::build(
            currency,
            rate_date,
            rate_to_mxn,
            source_name,
            source_url,
            fetched_at,
            false,
            None,
        )
    }

    pub fn manual(
        currency: impl Into<String>,
        rate_date: chrono::NaiveDate,
        rate_to_mxn: Decimal,
        source_name: impl Into<String>,
        source_url: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<Self, DomainError> {
        let reason = reason.into().trim().to_owned();
        if reason.is_empty() {
            return Err(DomainError::MissingManualRateReason);
        }
        Self::build(
            currency,
            rate_date,
            rate_to_mxn,
            source_name,
            source_url,
            Utc::now(),
            true,
            Some(reason),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        currency: impl Into<String>,
        rate_date: chrono::NaiveDate,
        rate_to_mxn: Decimal,
        source_name: impl Into<String>,
        source_url: impl Into<String>,
        fetched_at: DateTime<Utc>,
        is_manual: bool,
        manual_reason: Option<String>,
    ) -> Result<Self, DomainError> {
        let currency = normalize_currency(&currency.into())?;
        if rate_to_mxn <= Decimal::ZERO {
            return Err(DomainError::InvalidExchangeRate);
        }
        let source_name = source_name.into().trim().to_owned();
        if source_name.is_empty() {
            return Err(DomainError::MissingExchangeRateSource);
        }
        Ok(Self {
            currency,
            rate_date,
            rate_to_mxn,
            source_name,
            source_url: source_url.into().trim().to_owned(),
            fetched_at,
            is_manual,
            manual_reason,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductDraft {
    pub name: String,
    pub seller: Option<String>,
    pub quantity: u32,
    pub unit_price: Decimal,
    pub discount: Decimal,
    pub shipping: Decimal,
    pub taxes: Decimal,
    pub currency: String,
}

impl ProductDraft {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: impl Into<String>,
        seller: Option<String>,
        quantity: u32,
        unit_price: Decimal,
        discount: Decimal,
        shipping: Decimal,
        taxes: Decimal,
        currency: impl Into<String>,
    ) -> Result<Self, DomainError> {
        let name = name.into().trim().to_owned();
        if name.is_empty() {
            return Err(DomainError::MissingProductName);
        }
        if quantity == 0 {
            return Err(DomainError::InvalidProductQuantity);
        }
        if [unit_price, discount, shipping, taxes]
            .iter()
            .any(|amount| *amount < Decimal::ZERO)
        {
            return Err(DomainError::NegativeProductAmount);
        }
        Ok(Self {
            name,
            seller: normalize_optional(seller),
            quantity,
            unit_price,
            discount,
            shipping,
            taxes,
            currency: normalize_currency(&currency.into())?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductLine {
    pub id: Uuid,
    pub case_id: Uuid,
    pub name: String,
    pub seller: Option<String>,
    pub quantity: u32,
    pub unit_price: Decimal,
    pub discount: Decimal,
    pub shipping: Decimal,
    pub taxes: Decimal,
    pub currency: String,
    pub subtotal_original: Decimal,
    pub total_original: Decimal,
    pub total_mxn: Decimal,
    pub rate: ExchangeRateSnapshot,
    pub created_at: DateTime<Utc>,
}

impl ProductLine {
    pub fn new(
        case_id: Uuid,
        draft: ProductDraft,
        rate: ExchangeRateSnapshot,
    ) -> Result<Self, DomainError> {
        if draft.currency != rate.currency {
            return Err(DomainError::CurrencyRateMismatch);
        }
        let subtotal_original = draft.unit_price * Decimal::from(draft.quantity);
        // La valoración aduanera captura el costo de la mercancía y los impuestos
        // aplicables. El envío se conserva únicamente como referencia y los
        // descuentos no alteran el valor acreditado del producto.
        let total_original = subtotal_original + draft.taxes;
        let total_mxn = (total_original * rate.rate_to_mxn)
            .round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero);
        Ok(Self {
            id: Uuid::new_v4(),
            case_id,
            name: draft.name,
            seller: draft.seller,
            quantity: draft.quantity,
            unit_price: draft.unit_price,
            discount: draft.discount,
            shipping: draft.shipping,
            taxes: draft.taxes,
            currency: draft.currency,
            subtotal_original,
            total_original,
            total_mxn,
            rate,
            created_at: Utc::now(),
        })
    }
}

impl RectificationCase {
    pub fn new(
        tracking_number: impl Into<String>,
        customs_form_number: Option<String>,
        display_name: Option<String>,
    ) -> Result<Self, DomainError> {
        let tracking_number = normalize_tracking_number(&tracking_number.into())?;
        let customs_form_number = normalize_optional(customs_form_number);
        let display_name = normalize_optional(display_name)
            .unwrap_or_else(|| format!("Envío {}", tracking_number));
        let now = Utc::now();

        Ok(Self {
            id: Uuid::new_v4(),
            display_name,
            tracking_number,
            customs_form_number,
            status: CaseStatus::Draft,
            has_unseen_updates: false,
            archived_at: None,
            created_at: now,
            updated_at: now,
        })
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DomainError {
    #[error("El número de guía debe tener 13 caracteres alfanuméricos")]
    InvalidTrackingNumber,
    #[error("Estado de expediente desconocido: {0}")]
    UnknownCaseStatus(String),
    #[error("Tipo de evidencia desconocido: {0}")]
    UnknownEvidenceKind(String),
    #[error("La moneda debe ser un código ISO de tres letras")]
    InvalidCurrency,
    #[error("La tasa de conversión debe ser mayor que cero")]
    InvalidExchangeRate,
    #[error("La fuente del tipo de cambio es obligatoria")]
    MissingExchangeRateSource,
    #[error("Explica por qué se utilizó una tasa manual")]
    MissingManualRateReason,
    #[error("El nombre del producto es obligatorio")]
    MissingProductName,
    #[error("La cantidad debe ser al menos uno")]
    InvalidProductQuantity,
    #[error("Los importes del producto no pueden ser negativos")]
    NegativeProductAmount,
    #[error("El total del producto no puede ser negativo")]
    NegativeProductTotal,
    #[error("La moneda de la tasa no coincide con la del producto")]
    CurrencyRateMismatch,
}

pub fn normalize_tracking_number(value: &str) -> Result<String, DomainError> {
    let normalized = value.trim().to_ascii_uppercase();
    if normalized.len() != 13 || !normalized.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(DomainError::InvalidTrackingNumber);
    }
    Ok(normalized)
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

fn normalize_currency(value: &str) -> Result<String, DomainError> {
    let currency = value.trim().to_ascii_uppercase();
    if currency.len() != 3 || !currency.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err(DomainError::InvalidCurrency);
    }
    Ok(currency)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_an_international_tracking_number() {
        assert_eq!(
            normalize_tracking_number("  rr123456789mx ").unwrap(),
            "RR123456789MX"
        );
    }

    #[test]
    fn rejects_wrong_length_or_symbols() {
        assert_eq!(
            normalize_tracking_number("123").unwrap_err(),
            DomainError::InvalidTrackingNumber
        );
        assert_eq!(
            normalize_tracking_number("RR12345-789MX").unwrap_err(),
            DomainError::InvalidTrackingNumber
        );
    }

    #[test]
    fn calculates_the_percentage_added_by_an_excessive_customs_valuation() {
        let comparison =
            calculate_customs_overvaluation(Decimal::new(605_587, 2), Decimal::new(100_000, 2))
                .unwrap();

        assert_eq!(comparison.difference_mxn, Decimal::new(505_587, 2));
        assert_eq!(
            comparison.percentage_above_real_value,
            Decimal::new(50_559, 2)
        );
        assert!(
            calculate_customs_overvaluation(Decimal::new(100_000, 2), Decimal::new(100_000, 2))
                .is_none()
        );
    }

    #[test]
    fn creates_a_draft_with_a_fallback_name() {
        let case = RectificationCase::new("RR123456789MX", None, None).unwrap();
        assert_eq!(case.status, CaseStatus::Draft);
        assert_eq!(case.display_name, "Envío RR123456789MX");
        assert!(!case.has_unseen_updates);
    }

    #[test]
    fn excludes_discount_and_shipping_from_product_valuation() {
        let draft = ProductDraft::new(
            "Audífonos",
            Some("Tienda Japón".to_owned()),
            2,
            Decimal::new(1255, 2),
            Decimal::new(100, 2),
            Decimal::new(500, 2),
            Decimal::ZERO,
            "JPY",
        )
        .unwrap();
        let rate = ExchangeRateSnapshot::automatic(
            "JPY",
            chrono::NaiveDate::from_ymd_opt(2026, 8, 14).unwrap(),
            Decimal::new(10688, 5),
            "Proveedor de prueba",
            "https://example.test",
            Utc::now(),
        )
        .unwrap();
        let line = ProductLine::new(Uuid::new_v4(), draft, rate).unwrap();
        assert_eq!(line.subtotal_original, Decimal::new(2510, 2));
        assert_eq!(line.total_original, Decimal::new(2510, 2));
        assert_eq!(line.total_mxn, Decimal::new(268, 2));
        assert_eq!(line.shipping, Decimal::new(500, 2));
        assert_eq!(line.discount, Decimal::new(100, 2));
    }

    #[test]
    fn calculates_reproducible_multi_currency_fixtures() {
        let fixtures = [
            (
                "JPY", "32500.50", 2, "500.25", "1200", "0", "0.10688", "65001.00", "6947.31",
            ),
            (
                "USD", "19.99", 3, "5", "12.50", "2.33", "18.5432", "62.30", "1155.24",
            ),
        ];

        for (
            currency,
            unit_price,
            quantity,
            discount,
            shipping,
            taxes,
            rate,
            expected_original,
            expected_mxn,
        ) in fixtures
        {
            let draft = ProductDraft::new(
                "Producto fixture",
                None,
                quantity,
                Decimal::from_str(unit_price).unwrap(),
                Decimal::from_str(discount).unwrap(),
                Decimal::from_str(shipping).unwrap(),
                Decimal::from_str(taxes).unwrap(),
                currency,
            )
            .unwrap();
            let snapshot = ExchangeRateSnapshot::automatic(
                currency,
                chrono::NaiveDate::from_ymd_opt(2026, 8, 14).unwrap(),
                Decimal::from_str(rate).unwrap(),
                "Fixture auditable",
                "https://example.test/rate",
                Utc::now(),
            )
            .unwrap();
            let product = ProductLine::new(Uuid::new_v4(), draft, snapshot).unwrap();

            assert_eq!(
                product.total_original,
                Decimal::from_str(expected_original).unwrap()
            );
            assert_eq!(product.total_mxn, Decimal::from_str(expected_mxn).unwrap());
        }
    }

    #[test]
    fn requires_a_reason_for_manual_rates() {
        let result = ExchangeRateSnapshot::manual(
            "USD",
            chrono::NaiveDate::from_ymd_opt(2026, 8, 14).unwrap(),
            Decimal::new(1850, 2),
            "Documento adjunto",
            "",
            " ",
        );
        assert_eq!(result.unwrap_err(), DomainError::MissingManualRateReason);
    }
}
