use chrono::{NaiveDate, NaiveDateTime, Utc};
use mi_rectificacion_domain::{
    ExchangeRateSnapshot, TrackingEventInput, normalize_tracking_number,
};
use reqwest::blocking::Client;
use reqwest::header::{COOKIE, SET_COOKIE};
use rust_decimal::Decimal;
use serde_json::Value;
use std::collections::HashMap;
use std::{str::FromStr, time::Duration};
use thiserror::Error;

const FRANKFURTER_BASE_URL: &str = "https://api.frankfurter.dev/v1";

pub trait ExchangeRateProvider {
    fn rate_to_mxn(
        &self,
        currency: &str,
        date: NaiveDate,
    ) -> Result<ExchangeRateSnapshot, IntegrationError>;
}

pub const CORREOS_MEXICO_TRACKING_URL: &str =
    "https://www.correosdemexico.gob.mx/SSLServicios/SeguimientoEnvio/Seguimiento.aspx";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackingResponse {
    pub raw_response: String,
    pub events: Vec<TrackingEventInput>,
    pub fetched_at: chrono::DateTime<Utc>,
}

pub trait TrackingProvider {
    fn track(&self, tracking_number: &str, year: i32)
    -> Result<TrackingResponse, IntegrationError>;
}

#[derive(Debug, Clone)]
pub struct CorreosMexicoProvider {
    client: Client,
}

impl CorreosMexicoProvider {
    pub fn new() -> Result<Self, IntegrationError> {
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(18))
                .user_agent("MiRectificacionMX/0.1")
                .build()?,
        })
    }

    pub const fn portal_url() -> &'static str {
        CORREOS_MEXICO_TRACKING_URL
    }
}

impl TrackingProvider for CorreosMexicoProvider {
    fn track(
        &self,
        tracking_number: &str,
        year: i32,
    ) -> Result<TrackingResponse, IntegrationError> {
        let tracking_number = normalize_tracking_number(tracking_number)
            .map_err(|error| IntegrationError::InvalidTracking(error.to_string()))?;
        let initial = self
            .client
            .get(CORREOS_MEXICO_TRACKING_URL)
            .send()?
            .error_for_status()?;
        let cookie = initial
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .filter_map(|value| value.split(';').next())
            .collect::<Vec<_>>()
            .join("; ");
        let initial_html = initial.text()?;
        let viewstate = hidden_value(&initial_html, "__VIEWSTATE")?;
        let validation = hidden_value(&initial_html, "__EVENTVALIDATION")?;
        let generator = hidden_value(&initial_html, "__VIEWSTATEGENERATOR")?;
        let form = HashMap::from([
            ("__EVENTTARGET", "Busqueda".to_owned()),
            ("__EVENTARGUMENT", String::new()),
            ("__VIEWSTATE", viewstate),
            ("__VIEWSTATEGENERATOR", generator),
            ("__EVENTVALIDATION", validation),
            ("Guia", tracking_number),
            ("Periodo", year.to_string()),
        ]);
        let mut request = self.client.post(CORREOS_MEXICO_TRACKING_URL).form(&form);
        if !cookie.is_empty() {
            request = request.header(COOKIE, cookie);
        }
        let raw_response = request.send()?.error_for_status()?.text()?;
        if !raw_response.contains("Seguimiento") && !raw_response.contains("GDDatos") {
            return Err(IntegrationError::TrackingContractChanged);
        }
        Ok(TrackingResponse {
            events: parse_tracking_events(&raw_response),
            raw_response,
            fetched_at: Utc::now(),
        })
    }
}

fn hidden_value(html: &str, name: &str) -> Result<String, IntegrationError> {
    let lower = html.to_ascii_lowercase();
    let needle = format!("name=\"{}\"", name.to_ascii_lowercase());
    let name_index = lower
        .find(&needle)
        .ok_or(IntegrationError::TrackingContractChanged)?;
    let input_start = lower[..name_index]
        .rfind("<input")
        .ok_or(IntegrationError::TrackingContractChanged)?;
    let input_end = lower[name_index..]
        .find('>')
        .map(|offset| name_index + offset)
        .ok_or(IntegrationError::TrackingContractChanged)?;
    let input = &html[input_start..=input_end];
    attribute_value(input, "value").ok_or(IntegrationError::TrackingContractChanged)
}

fn attribute_value(tag: &str, attribute: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    for quote in ['"', '\''] {
        let needle = format!("{}={quote}", attribute.to_ascii_lowercase());
        if let Some(start) = lower.find(&needle) {
            let value_start = start + needle.len();
            let value_end = tag[value_start..].find(quote)? + value_start;
            return Some(decode_html(&tag[value_start..value_end]));
        }
    }
    None
}

pub fn parse_tracking_events(html: &str) -> Vec<TrackingEventInput> {
    let Some(table) = table_by_id(html, "GDDatos") else {
        return Vec::new();
    };
    let rows = split_elements(table, "tr");
    let headers = rows
        .iter()
        .find_map(|row| {
            let values = split_elements(row, "th")
                .into_iter()
                .map(strip_html)
                .collect::<Vec<_>>();
            (!values.is_empty()).then_some(values)
        })
        .unwrap_or_default();
    let header_index = |names: &[&str]| {
        headers.iter().position(|header| {
            let header = header.trim().to_lowercase();
            names.iter().any(|name| header == *name)
        })
    };
    let date_index = header_index(&["fecha"]).unwrap_or(0);
    let time_index = header_index(&["hora"]);
    let location_index = header_index(&["origen", "ubicación", "ubicacion"]);
    let event_index = header_index(&["evento"]);

    rows.into_iter()
        .filter_map(|row| {
            let cells = split_elements(row, "td")
                .into_iter()
                .map(strip_html)
                .collect::<Vec<_>>();
            if cells.len() < 2 {
                return None;
            }
            let description_index = event_index.unwrap_or(cells.len() - 1);
            let description = cells
                .get(description_index)
                .map(|value| polish_portal_spanish(value))?;
            if description.is_empty() {
                return None;
            }
            let date = cells
                .get(date_index)
                .map(String::as_str)
                .unwrap_or_default();
            let occurred_at = time_index
                .and_then(|index| cells.get(index))
                .map(|time| format!("{date} {time}"))
                .and_then(|value| parse_portal_datetime(&value))
                .or_else(|| parse_portal_datetime(date));
            let location = location_index
                .and_then(|index| cells.get(index))
                .map(|value| polish_portal_spanish(value))
                .filter(|value| !value.is_empty());
            Some(TrackingEventInput {
                occurred_at,
                description,
                location,
            })
        })
        .collect()
}

fn table_by_id<'a>(html: &'a str, id: &str) -> Option<&'a str> {
    let lower = html.to_ascii_lowercase();
    let id_double = format!("id=\"{}\"", id.to_ascii_lowercase());
    let id_single = format!("id='{}'", id.to_ascii_lowercase());
    let id_index = lower.find(&id_double).or_else(|| lower.find(&id_single))?;
    let start = lower[..id_index].rfind("<table")?;
    let end = lower[id_index..].find("</table>")? + id_index + "</table>".len();
    Some(&html[start..end])
}

fn split_elements<'a>(html: &'a str, tag: &str) -> Vec<&'a str> {
    let lower = html.to_ascii_lowercase();
    let opening = format!("<{tag}");
    let closing = format!("</{tag}>");
    let mut items = Vec::new();
    let mut cursor = 0;
    while let Some(relative_start) = lower[cursor..].find(&opening) {
        let start = cursor + relative_start;
        let Some(relative_end) = lower[start..].find(&closing) else {
            break;
        };
        let end = start + relative_end + closing.len();
        items.push(&html[start..end]);
        cursor = end;
    }
    items
}

fn strip_html(value: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    for character in value.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(character),
            _ => {}
        }
    }
    decode_html(&text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn decode_html(value: &str) -> String {
    let value = value
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">");
    let mut decoded = String::with_capacity(value.len());
    let mut remainder = value.as_str();
    while let Some(start) = remainder.find("&#") {
        decoded.push_str(&remainder[..start]);
        let entity = &remainder[start..];
        let Some(end) = entity.find(';') else {
            decoded.push_str(entity);
            return decoded;
        };
        let number = &entity[2..end];
        let codepoint = number
            .strip_prefix(['x', 'X'])
            .and_then(|value| u32::from_str_radix(value, 16).ok())
            .or_else(|| number.parse::<u32>().ok());
        if let Some(character) = codepoint.and_then(char::from_u32) {
            decoded.push(character);
        } else {
            decoded.push_str(&entity[..=end]);
        }
        remainder = &entity[end + 1..];
    }
    decoded.push_str(remainder);
    decoded
}

fn polish_portal_spanish(value: &str) -> String {
    value
        .trim()
        .replace(
            "Deposito del Cliente en Japan",
            "Depósito del cliente en Japón",
        )
        .replace("1er visita", "Primera visita")
        .replace("país destino", "país de destino")
        .replace("previo Aduana", "previo a la Aduana")
        .replace("Cd Obregón", "Cd. Obregón")
}

fn parse_portal_datetime(value: &str) -> Option<chrono::DateTime<Utc>> {
    for format in ["%d/%m/%Y %H:%M:%S", "%d/%m/%Y %H:%M"] {
        if let Ok(value) = NaiveDateTime::parse_from_str(value.trim(), format) {
            return Some(value.and_utc());
        }
    }
    NaiveDate::parse_from_str(value.trim(), "%d/%m/%Y")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|value| value.and_utc())
}

#[derive(Debug, Clone)]
pub struct FrankfurterProvider {
    client: Client,
}

impl FrankfurterProvider {
    pub fn new() -> Result<Self, IntegrationError> {
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(12))
                .user_agent("MiRectificacionMX/0.1")
                .build()?,
        })
    }
}

impl ExchangeRateProvider for FrankfurterProvider {
    fn rate_to_mxn(
        &self,
        currency: &str,
        date: NaiveDate,
    ) -> Result<ExchangeRateSnapshot, IntegrationError> {
        let currency = currency.trim().to_ascii_uppercase();
        if currency == "MXN" {
            return ExchangeRateSnapshot::automatic(
                "MXN",
                date,
                Decimal::ONE,
                "Moneda nacional",
                "local://mxn",
                Utc::now(),
            )
            .map_err(|error| IntegrationError::InvalidResponse(error.to_string()));
        }
        if currency.len() != 3 || !currency.bytes().all(|byte| byte.is_ascii_uppercase()) {
            return Err(IntegrationError::UnsupportedCurrency(currency));
        }

        let url = frankfurter_rate_url(&currency, date);
        let response = self.client.get(&url).send()?.error_for_status()?;
        let payload: Value = response.json()?;
        let response_date = payload
            .get("date")
            .and_then(Value::as_str)
            .ok_or(IntegrationError::MissingRate)?;
        let effective_date = NaiveDate::parse_from_str(response_date, "%Y-%m-%d")
            .map_err(|_| IntegrationError::MissingRate)?;
        let rate_text = payload
            .get("rates")
            .and_then(|rates| rates.get("MXN"))
            .map(Value::to_string)
            .ok_or(IntegrationError::MissingRate)?;
        let rate = Decimal::from_str(&rate_text).map_err(|_| IntegrationError::MissingRate)?;

        ExchangeRateSnapshot::automatic(
            currency,
            effective_date,
            rate,
            "Frankfurter / bancos centrales",
            url,
            Utc::now(),
        )
        .map_err(|error| IntegrationError::InvalidResponse(error.to_string()))
    }
}

fn frankfurter_rate_url(currency: &str, date: NaiveDate) -> String {
    format!("{FRANKFURTER_BASE_URL}/{date}?from={currency}&to=MXN")
}

#[derive(Debug, Error)]
pub enum IntegrationError {
    #[error(
        "No pudimos conectarnos al servicio de tipos de cambio. Revisa tu conexión e inténtalo de nuevo"
    )]
    Http(
        #[from]
        #[source]
        reqwest::Error,
    ),
    #[error("El proveedor no devolvió una tasa MXN")]
    MissingRate,
    #[error("Moneda no admitida: {0}")]
    UnsupportedCurrency(String),
    #[error("Respuesta de tasa inválida: {0}")]
    InvalidResponse(String),
    #[error("Número de guía inválido: {0}")]
    InvalidTracking(String),
    #[error("Correos de México cambió el contrato público de rastreo")]
    TrackingContractChanged,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mxn_uses_an_exact_local_rate_without_network() {
        let provider = FrankfurterProvider::new().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 8, 14).unwrap();
        let snapshot = provider.rate_to_mxn("mxn", date).unwrap();
        assert_eq!(snapshot.rate_to_mxn, Decimal::ONE);
        assert_eq!(snapshot.currency, "MXN");
        assert!(!snapshot.is_manual);
    }

    #[test]
    fn uses_the_current_frankfurter_api_endpoint() {
        let date = NaiveDate::from_ymd_opt(2026, 8, 15).unwrap();
        assert_eq!(
            frankfurter_rate_url("JPY", date),
            "https://api.frankfurter.dev/v1/2026-08-15?from=JPY&to=MXN"
        );
    }

    #[test]
    fn parses_tracking_rows_from_the_public_portal_shape() {
        let html = r#"
            <table id="GDDatos">
              <tr><th>Fecha</th><th>Ubicación</th><th>Evento</th></tr>
              <tr><td>12/08/2026 09:30</td><td>CDMX</td><td>Recibido en oficina postal</td></tr>
              <tr><td>13/08/2026 18:02:10</td><td>Centro Operativo</td><td>En tránsito</td></tr>
            </table>
        "#;
        let events = parse_tracking_events(html);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].description, "Recibido en oficina postal");
        assert_eq!(events[0].location.as_deref(), Some("CDMX"));
        assert_eq!(
            events[1]
                .occurred_at
                .unwrap()
                .format("%d/%m/%Y %H:%M:%S")
                .to_string(),
            "13/08/2026 18:02:10"
        );
    }

    #[test]
    fn reads_aspnet_hidden_fields_without_network() {
        let html =
            r#"<input type="hidden" name="__VIEWSTATE" id="__VIEWSTATE" value="abc+123=" />"#;
        assert_eq!(hidden_value(html, "__VIEWSTATE").unwrap(), "abc+123=");
    }

    #[test]
    fn parses_the_numbered_portal_shape_with_correct_spanish_and_time() {
        let html = r#"
            <table id="GDDatos">
              <tr><th>Fecha</th><th>Hora</th><th>Origen</th><th>Evento</th><th>&nbsp;</th></tr>
              <tr><td><b>14/08/2026</b></td><td>13:24:00</td><td>Administraci&#243;n Postal Cd Obreg&#243;n, Son.</td><td>1er visita al domicilio para entrega, no se encontr&#243; al destinatario</td><td><font>7</font></td></tr>
            </table>
        "#;
        let events = parse_tracking_events(html);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].description,
            "Primera visita al domicilio para entrega, no se encontró al destinatario"
        );
        assert_eq!(
            events[0].location.as_deref(),
            Some("Administración Postal Cd. Obregón, Son.")
        );
        assert_eq!(
            events[0]
                .occurred_at
                .unwrap()
                .format("%d/%m/%Y %H:%M:%S")
                .to_string(),
            "14/08/2026 13:24:00"
        );
    }
}
