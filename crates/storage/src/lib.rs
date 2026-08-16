use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, NaiveDate, Utc};
use directories::ProjectDirs;
use mi_rectificacion_application::CaseRepository;
use mi_rectificacion_domain::{
    ApplicantProfile, AuditEvent, CaseStatus, EmailDraft, EvidenceDocument, EvidenceKind,
    ExchangeRateSnapshot, MAX_EVIDENCE_SIZE_BYTES, ProductDraft, ProductLine, RectificationCase,
    TrackingEvent, TrackingEventInput, TrackingRefreshState,
};
use mi_rectificacion_security::AttachmentCipher;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use rust_decimal::Decimal;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    str::FromStr,
};
use uuid::Uuid;

const INITIAL_MIGRATION: &str = include_str!("../migrations/0001_initial.sql");
const EVIDENCE_MIGRATION: &str = include_str!("../migrations/0002_evidence_and_audit.sql");
const PRODUCTS_MIGRATION: &str = include_str!("../migrations/0003_products_and_rates.sql");
const TRACKING_MIGRATION: &str = include_str!("../migrations/0004_tracking.sql");
const TRACKING_PARSER_FIX_MIGRATION: &str =
    include_str!("../migrations/0005_tracking_parser_fix.sql");
const APPLICANT_PROFILE_MIGRATION: &str = include_str!("../migrations/0006_applicant_profile.sql");
const EMAIL_DRAFTS_MIGRATION: &str = include_str!("../migrations/0007_email_drafts.sql");
const ONBOARDING_MIGRATION: &str = include_str!("../migrations/0008_onboarding.sql");
const ARCHIVED_CASES_MIGRATION: &str = include_str!("../migrations/0009_archived_cases.sql");
const CAPTURE_PROGRESS_MIGRATION: &str = include_str!("../migrations/0010_capture_progress.sql");
const CUSTOMS_VALUATION_MIGRATION: &str = include_str!("../migrations/0011_customs_valuation.sql");
const DATABASE_FILENAME: &str = "rectifications.db";

fn app_project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("mx", "Mi Rectificacion MX", "Mi Rectificacion MX")
        .context("No se pudo resolver el directorio de datos de la aplicación")
}

#[derive(Debug, Clone)]
pub struct SqliteCaseRepository {
    database_path: PathBuf,
}

impl SqliteCaseRepository {
    pub fn open_default() -> Result<Self> {
        let project_dirs = app_project_dirs()?;
        Self::open(project_dirs.data_local_dir().join(DATABASE_FILENAME))
    }

    pub fn open(database_path: impl Into<PathBuf>) -> Result<Self> {
        let database_path = database_path.into();
        if let Some(parent) = database_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("No se pudo crear el directorio {}", parent.display()))?;
        }

        let repository = Self { database_path };
        repository.migrate()?;
        Ok(repository)
    }

    fn connection(&self) -> Result<Connection> {
        let connection = Connection::open(&self.database_path)
            .with_context(|| format!("No se pudo abrir {}", self.database_path.display()))?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .context("No se pudieron activar las llaves foráneas")?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .context("No se pudo activar el journal WAL")?;
        Ok(connection)
    }

    fn migrate(&self) -> Result<()> {
        self.connection()?
            .execute_batch(&format!(
                "{INITIAL_MIGRATION}\n{EVIDENCE_MIGRATION}\n{PRODUCTS_MIGRATION}\n{TRACKING_MIGRATION}\n{TRACKING_PARSER_FIX_MIGRATION}\n{APPLICANT_PROFILE_MIGRATION}\n{EMAIL_DRAFTS_MIGRATION}\n{ONBOARDING_MIGRATION}\n{ARCHIVED_CASES_MIGRATION}\n{CAPTURE_PROGRESS_MIGRATION}\n{CUSTOMS_VALUATION_MIGRATION}"
            ))
            .context("Fallaron las migraciones de SQLite")
    }

    fn find(&self, case_id: Uuid) -> Result<RectificationCase> {
        self.list()?
            .into_iter()
            .find(|case| case.id == case_id)
            .context("El expediente no existe")
    }
}

impl CaseRepository for SqliteCaseRepository {
    type Error = anyhow::Error;

    fn list(&self) -> Result<Vec<RectificationCase>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT cases.id, cases.display_name, cases.tracking_number,
                    cases.customs_form_number, cases.status,
                    cases.has_unseen_updates, archived.archived_at,
                    cases.created_at, cases.updated_at
             FROM rectification_cases AS cases
             LEFT JOIN archived_cases AS archived ON archived.case_id = cases.id
             ORDER BY cases.updated_at DESC",
        )?;

        let rows = statement.query_map([], |row| {
            let id: String = row.get(0)?;
            let status: String = row.get(4)?;
            let archived_at: Option<String> = row.get(6)?;
            let created_at: String = row.get(7)?;
            let updated_at: String = row.get(8)?;
            Ok((
                id,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                status,
                row.get::<_, bool>(5)?,
                archived_at,
                created_at,
                updated_at,
            ))
        })?;

        rows.map(|row| {
            let (
                id,
                display_name,
                tracking_number,
                customs_form_number,
                status,
                has_unseen_updates,
                archived_at,
                created_at,
                updated_at,
            ) = row?;
            Ok(RectificationCase {
                id: Uuid::parse_str(&id).context("ID de expediente inválido")?,
                display_name,
                tracking_number,
                customs_form_number,
                status: CaseStatus::from_str(&status)?,
                has_unseen_updates,
                archived_at: archived_at
                    .as_deref()
                    .map(|value| parse_datetime(value, "Fecha de archivado inválida"))
                    .transpose()?,
                created_at: parse_datetime(&created_at, "Fecha de creación inválida")?,
                updated_at: parse_datetime(&updated_at, "Fecha de actualización inválida")?,
            })
        })
        .collect()
    }

    fn insert(&self, case: &RectificationCase) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO rectification_cases (
                id, display_name, tracking_number, customs_form_number, status,
                has_unseen_updates, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                case.id.to_string(),
                case.display_name,
                case.tracking_number,
                case.customs_form_number,
                case.status.as_str(),
                case.has_unseen_updates,
                case.created_at.to_rfc3339(),
                case.updated_at.to_rfc3339(),
            ],
        )?;
        insert_audit_event(&transaction, case.id, "case_created", "Expediente creado")?;
        transaction.commit()?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct CaseDataStore {
    repository: SqliteCaseRepository,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackingUpdateResult {
    pub inserted_events: usize,
    pub total_events: usize,
}

impl CaseDataStore {
    pub fn open_default() -> Result<Self> {
        Ok(Self {
            repository: SqliteCaseRepository::open_default()?,
        })
    }

    pub fn open(repository: SqliteCaseRepository) -> Self {
        Self { repository }
    }

    pub fn load_applicant_profile(&self) -> Result<ApplicantProfile> {
        self.repository
            .connection()?
            .query_row(
                "SELECT full_name, email, phone, address, city, state, postal_code
             FROM applicant_profile WHERE singleton_id = 1",
                [],
                |row| {
                    Ok(ApplicantProfile {
                        full_name: row.get(0)?,
                        email: row.get(1)?,
                        phone: row.get(2)?,
                        address: row.get(3)?,
                        city: row.get(4)?,
                        state: row.get(5)?,
                        postal_code: row.get(6)?,
                    })
                },
            )
            .context("No se pudo cargar el perfil del solicitante")
    }

    pub fn save_applicant_profile(&self, profile: &ApplicantProfile) -> Result<()> {
        self.repository.connection()?.execute(
            "INSERT INTO applicant_profile (
                singleton_id, full_name, email, phone, address, city, state,
                postal_code, updated_at
             ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(singleton_id) DO UPDATE SET
                full_name = excluded.full_name,
                email = excluded.email,
                phone = excluded.phone,
                address = excluded.address,
                city = excluded.city,
                state = excluded.state,
                postal_code = excluded.postal_code,
                updated_at = excluded.updated_at",
            params![
                profile.full_name.trim(),
                profile.email.trim(),
                profile.phone.trim(),
                profile.address.trim(),
                profile.city.trim(),
                profile.state.trim(),
                profile.postal_code.trim(),
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn load_customs_valuation(&self, case_id: Uuid) -> Result<Option<Decimal>> {
        self.repository.find(case_id)?;
        let value = self
            .repository
            .connection()?
            .query_row(
                "SELECT presumptive_value_mxn FROM case_customs_valuation WHERE case_id = ?1",
                [case_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        value
            .map(|value| {
                Decimal::from_str(&value).context("La valuación aduanera guardada no es válida")
            })
            .transpose()
    }

    pub fn save_customs_valuation(&self, case_id: Uuid, value_mxn: Decimal) -> Result<()> {
        self.repository.find(case_id)?;
        if value_mxn <= Decimal::ZERO {
            bail!("La valuación aduanera debe ser mayor que cero");
        }
        self.repository.connection()?.execute(
            "INSERT INTO case_customs_valuation (case_id, presumptive_value_mxn, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(case_id) DO UPDATE SET
                presumptive_value_mxn = excluded.presumptive_value_mxn,
                updated_at = excluded.updated_at",
            params![
                case_id.to_string(),
                value_mxn.normalize().to_string(),
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn automatic_email_documents_root(&self, case_id: Uuid) -> Result<PathBuf> {
        self.repository.find(case_id)?;
        let database_directory = self
            .repository
            .database_path
            .parent()
            .context("La ruta de la base de datos local no es válida")?;
        let directory = database_directory
            .join("generated_documents")
            .join(case_id.to_string());
        fs::create_dir_all(&directory)
            .with_context(|| format!("No se pudo crear {}", directory.display()))?;
        Ok(directory)
    }

    pub fn is_onboarding_completed(&self) -> Result<bool> {
        self.repository
            .connection()?
            .query_row(
                "SELECT onboarding_completed FROM app_preferences WHERE singleton_id = 1",
                [],
                |row| row.get(0),
            )
            .context("No se pudo consultar el estado del recorrido inicial")
    }

    pub fn complete_onboarding(&self, profile: &ApplicantProfile) -> Result<()> {
        let mut connection = self.repository.connection()?;
        let transaction = connection.transaction()?;
        let completed_at = Utc::now().to_rfc3339();
        transaction.execute(
            "INSERT INTO applicant_profile (
                singleton_id, full_name, email, phone, address, city, state,
                postal_code, updated_at
             ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(singleton_id) DO UPDATE SET
                full_name = excluded.full_name,
                email = excluded.email,
                phone = excluded.phone,
                address = excluded.address,
                city = excluded.city,
                state = excluded.state,
                postal_code = excluded.postal_code,
                updated_at = excluded.updated_at",
            params![
                profile.full_name.trim(),
                profile.email.trim(),
                profile.phone.trim(),
                profile.address.trim(),
                profile.city.trim(),
                profile.state.trim(),
                profile.postal_code.trim(),
                completed_at,
            ],
        )?;
        transaction.execute(
            "UPDATE app_preferences
             SET onboarding_completed = 1, onboarding_completed_at = ?1
             WHERE singleton_id = 1",
            [completed_at],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn set_case_archived(&self, case_id: Uuid, archived: bool) -> Result<()> {
        self.repository.find(case_id)?;
        let now = Utc::now();
        let mut connection = self.repository.connection()?;
        let transaction = connection.transaction()?;

        if archived {
            transaction.execute(
                "INSERT INTO archived_cases (case_id, archived_at)
                 VALUES (?1, ?2)
                 ON CONFLICT(case_id) DO UPDATE SET archived_at = excluded.archived_at",
                params![case_id.to_string(), now.to_rfc3339()],
            )?;
        } else {
            transaction.execute(
                "DELETE FROM archived_cases WHERE case_id = ?1",
                [case_id.to_string()],
            )?;
        }

        transaction.execute(
            "UPDATE rectification_cases SET updated_at = ?1 WHERE id = ?2",
            params![now.to_rfc3339(), case_id.to_string()],
        )?;
        insert_audit_event(
            &transaction,
            case_id,
            if archived {
                "case_archived"
            } else {
                "case_restored"
            },
            if archived {
                "Expediente archivado"
            } else {
                "Expediente restaurado"
            },
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn save_capture_progress(&self, case_id: Uuid, current_step: usize) -> Result<()> {
        self.repository.find(case_id)?;
        if !(2..=5).contains(&current_step) {
            bail!("Paso de captura inválido");
        }
        self.repository.connection()?.execute(
            "INSERT INTO case_capture_progress (case_id, current_step, updated_at, completed_at)
             VALUES (?1, ?2, ?3, NULL)
             ON CONFLICT(case_id) DO UPDATE SET
                current_step = excluded.current_step,
                updated_at = excluded.updated_at,
                completed_at = NULL",
            params![case_id.to_string(), current_step, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn complete_capture(&self, case_id: Uuid) -> Result<()> {
        self.repository.find(case_id)?;
        let now = Utc::now().to_rfc3339();
        self.repository.connection()?.execute(
            "UPDATE case_capture_progress
             SET updated_at = ?1, completed_at = ?1
             WHERE case_id = ?2",
            params![now, case_id.to_string()],
        )?;
        Ok(())
    }

    pub fn incomplete_capture(&self) -> Result<Option<(Uuid, usize)>> {
        let connection = self.repository.connection()?;
        let mut statement = connection.prepare(
            "SELECT progress.case_id, progress.current_step
             FROM case_capture_progress AS progress
             INNER JOIN rectification_cases AS cases ON cases.id = progress.case_id
             LEFT JOIN archived_cases AS archived ON archived.case_id = progress.case_id
             WHERE progress.completed_at IS NULL AND archived.case_id IS NULL
             ORDER BY progress.updated_at DESC LIMIT 1",
        )?;
        let mut rows = statement.query([])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let case_id: String = row.get(0)?;
        Ok(Some((Uuid::parse_str(&case_id)?, row.get(1)?)))
    }

    pub fn load_email_draft(&self, case_id: Uuid) -> Result<Option<EmailDraft>> {
        self.repository.find(case_id)?;
        let connection = self.repository.connection()?;
        let mut statement = connection.prepare(
            "SELECT recipient, sender, subject, body, request_pdf_path,
                    evidence_pdf_path, eml_path, prepared_at, opened_at, sent_at
             FROM email_drafts WHERE case_id = ?1",
        )?;
        let mut rows = statement.query([case_id.to_string()])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let prepared_at: String = row.get(7)?;
        let opened_at: Option<String> = row.get(8)?;
        let sent_at: Option<String> = row.get(9)?;
        Ok(Some(EmailDraft {
            case_id,
            recipient: row.get(0)?,
            sender: row.get(1)?,
            subject: row.get(2)?,
            body: row.get(3)?,
            request_pdf_path: row.get(4)?,
            evidence_pdf_path: row.get(5)?,
            eml_path: row.get(6)?,
            prepared_at: parse_datetime(&prepared_at, "Fecha de preparación inválida")?,
            opened_at: opened_at
                .as_deref()
                .map(|value| parse_datetime(value, "Fecha de apertura inválida"))
                .transpose()?,
            sent_at: sent_at
                .as_deref()
                .map(|value| parse_datetime(value, "Fecha de envío inválida"))
                .transpose()?,
        }))
    }

    pub fn save_email_draft(&self, draft: &EmailDraft) -> Result<()> {
        self.repository.find(draft.case_id)?;
        let mut connection = self.repository.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO email_drafts (
                case_id, recipient, sender, subject, body, request_pdf_path,
                evidence_pdf_path, eml_path, prepared_at, opened_at, sent_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(case_id) DO UPDATE SET
                recipient = excluded.recipient,
                sender = excluded.sender,
                subject = excluded.subject,
                body = excluded.body,
                request_pdf_path = excluded.request_pdf_path,
                evidence_pdf_path = excluded.evidence_pdf_path,
                eml_path = excluded.eml_path,
                prepared_at = excluded.prepared_at,
                opened_at = excluded.opened_at,
                sent_at = excluded.sent_at",
            params![
                draft.case_id.to_string(),
                draft.recipient.trim(),
                draft.sender.trim(),
                draft.subject.trim(),
                draft.body.trim(),
                draft.request_pdf_path,
                draft.evidence_pdf_path,
                draft.eml_path,
                draft.prepared_at.to_rfc3339(),
                draft.opened_at.map(|value| value.to_rfc3339()),
                draft.sent_at.map(|value| value.to_rfc3339()),
            ],
        )?;
        transaction.execute(
            "UPDATE rectification_cases SET status = 'email_prepared', updated_at = ?1 WHERE id = ?2",
            params![draft.prepared_at.to_rfc3339(), draft.case_id.to_string()],
        )?;
        insert_audit_event(
            &transaction,
            draft.case_id,
            "email_prepared",
            "Borrador de correo preparado para revisión",
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn record_email_opened(&self, case_id: Uuid) -> Result<()> {
        let now = Utc::now();
        let mut connection = self.repository.connection()?;
        let transaction = connection.transaction()?;
        let updated = transaction.execute(
            "UPDATE email_drafts SET opened_at = ?1 WHERE case_id = ?2",
            params![now.to_rfc3339(), case_id.to_string()],
        )?;
        if updated == 0 {
            bail!("Prepara el borrador antes de abrirlo");
        }
        insert_audit_event(
            &transaction,
            case_id,
            "email_opened",
            "Borrador abierto en el cliente de correo",
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn mark_email_sent(&self, case_id: Uuid) -> Result<()> {
        let now = Utc::now();
        let mut connection = self.repository.connection()?;
        let transaction = connection.transaction()?;
        let updated = transaction.execute(
            "UPDATE email_drafts SET sent_at = ?1 WHERE case_id = ?2",
            params![now.to_rfc3339(), case_id.to_string()],
        )?;
        if updated == 0 {
            bail!("Prepara el borrador antes de marcarlo como enviado");
        }
        transaction.execute(
            "UPDATE rectification_cases SET status = 'sent', updated_at = ?1 WHERE id = ?2",
            params![now.to_rfc3339(), case_id.to_string()],
        )?;
        insert_audit_event(
            &transaction,
            case_id,
            "email_sent",
            "Correo marcado manualmente como enviado",
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn list_tracking_events(&self, case_id: Uuid) -> Result<Vec<TrackingEvent>> {
        let connection = self.repository.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, case_id, fingerprint, occurred_at, description, location,
                    source, is_seen, created_at
             FROM tracking_events
             WHERE case_id = ?1
             ORDER BY occurred_at DESC, created_at DESC, id DESC",
        )?;
        let rows = statement.query_map([case_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, bool>(7)?,
                row.get::<_, String>(8)?,
            ))
        })?;
        rows.map(|row| {
            let (
                id,
                case_id,
                fingerprint,
                occurred_at,
                description,
                location,
                source,
                is_seen,
                created_at,
            ) = row?;
            Ok(TrackingEvent {
                id: Uuid::parse_str(&id)?,
                case_id: Uuid::parse_str(&case_id)?,
                fingerprint,
                occurred_at: occurred_at
                    .as_deref()
                    .map(|value| parse_datetime(value, "Fecha de movimiento inválida"))
                    .transpose()?,
                description,
                location,
                source,
                is_seen,
                created_at: parse_datetime(&created_at, "Fecha de rastreo inválida")?,
            })
        })
        .collect()
    }

    pub fn tracking_refresh_state(&self, case_id: Uuid) -> Result<TrackingRefreshState> {
        let connection = self.repository.connection()?;
        let mut statement = connection.prepare(
            "SELECT last_attempt_at, last_success_at, last_error
             FROM tracking_refresh_state WHERE case_id = ?1",
        )?;
        let mut rows = statement.query([case_id.to_string()])?;
        let Some(row) = rows.next()? else {
            return Ok(TrackingRefreshState::default());
        };
        let last_attempt_at = row.get::<_, Option<String>>(0)?;
        let last_success_at = row.get::<_, Option<String>>(1)?;
        Ok(TrackingRefreshState {
            last_attempt_at: last_attempt_at
                .as_deref()
                .map(|value| parse_datetime(value, "Fecha de intento inválida"))
                .transpose()?,
            last_success_at: last_success_at
                .as_deref()
                .map(|value| parse_datetime(value, "Fecha de consulta inválida"))
                .transpose()?,
            last_error: row.get(2)?,
        })
    }

    pub fn record_tracking_response(
        &self,
        case_id: Uuid,
        provider: &str,
        raw_response: &str,
        fetched_at: DateTime<Utc>,
        events: &[TrackingEventInput],
    ) -> Result<TrackingUpdateResult> {
        self.repository.find(case_id)?;
        let mut connection = self.repository.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO tracking_snapshots (
                id, case_id, provider, fetched_at, raw_response, error_message
             ) VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            params![
                Uuid::new_v4().to_string(),
                case_id.to_string(),
                provider,
                fetched_at.to_rfc3339(),
                raw_response,
            ],
        )?;
        let mut inserted_events = 0;
        for event in events {
            let description = event.description.trim();
            if description.is_empty() {
                continue;
            }
            let fingerprint = tracking_fingerprint(event);
            inserted_events += transaction.execute(
                "INSERT OR IGNORE INTO tracking_events (
                    id, case_id, fingerprint, occurred_at, description, location,
                    source, is_seen, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8)",
                params![
                    Uuid::new_v4().to_string(),
                    case_id.to_string(),
                    fingerprint,
                    event.occurred_at.map(|value| value.to_rfc3339()),
                    description,
                    event.location.as_deref().map(str::trim),
                    provider,
                    fetched_at.to_rfc3339(),
                ],
            )?;
        }
        transaction.execute(
            "INSERT INTO tracking_refresh_state (
                case_id, last_attempt_at, last_success_at, last_error
             ) VALUES (?1, ?2, ?2, NULL)
             ON CONFLICT(case_id) DO UPDATE SET
                last_attempt_at = excluded.last_attempt_at,
                last_success_at = excluded.last_success_at,
                last_error = NULL",
            params![case_id.to_string(), fetched_at.to_rfc3339()],
        )?;
        if inserted_events > 0 {
            transaction.execute(
                "UPDATE rectification_cases
                 SET has_unseen_updates = 1, updated_at = ?1
                 WHERE id = ?2",
                params![fetched_at.to_rfc3339(), case_id.to_string()],
            )?;
            insert_audit_event(
                &transaction,
                case_id,
                "tracking_updated",
                &format!("Rastreo actualizado: {inserted_events} movimiento(s) nuevo(s)"),
            )?;
        }
        let total_events = transaction.query_row(
            "SELECT COUNT(*) FROM tracking_events WHERE case_id = ?1",
            [case_id.to_string()],
            |row| row.get::<_, usize>(0),
        )?;
        transaction.commit()?;
        Ok(TrackingUpdateResult {
            inserted_events,
            total_events,
        })
    }

    pub fn record_tracking_error(
        &self,
        case_id: Uuid,
        provider: &str,
        raw_response: &str,
        error: &str,
        attempted_at: DateTime<Utc>,
    ) -> Result<()> {
        self.repository.find(case_id)?;
        let mut connection = self.repository.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO tracking_snapshots (
                id, case_id, provider, fetched_at, raw_response, error_message
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                Uuid::new_v4().to_string(),
                case_id.to_string(),
                provider,
                attempted_at.to_rfc3339(),
                raw_response,
                error,
            ],
        )?;
        transaction.execute(
            "INSERT INTO tracking_refresh_state (
                case_id, last_attempt_at, last_success_at, last_error
             ) VALUES (?1, ?2, NULL, ?3)
             ON CONFLICT(case_id) DO UPDATE SET
                last_attempt_at = excluded.last_attempt_at,
                last_error = excluded.last_error",
            params![case_id.to_string(), attempted_at.to_rfc3339(), error],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn add_manual_tracking_event(
        &self,
        case_id: Uuid,
        event: TrackingEventInput,
    ) -> Result<TrackingUpdateResult> {
        let raw_response = serde_json::to_string(&event)?;
        self.record_tracking_response(case_id, "manual", &raw_response, Utc::now(), &[event])
    }

    pub fn mark_tracking_seen(&self, case_id: Uuid) -> Result<()> {
        let mut connection = self.repository.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE tracking_events SET is_seen = 1 WHERE case_id = ?1",
            [case_id.to_string()],
        )?;
        transaction.execute(
            "UPDATE rectification_cases
             SET has_unseen_updates = 0
             WHERE id = ?1",
            [case_id.to_string()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn list_products(&self, case_id: Uuid) -> Result<Vec<ProductLine>> {
        let connection = self.repository.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, case_id, name, seller, quantity, unit_price, discount, shipping,
                    taxes, currency, subtotal_original, total_original, total_mxn,
                    rate_date, rate_to_mxn, rate_source_name, rate_source_url,
                    rate_fetched_at, rate_is_manual, manual_rate_reason, created_at
             FROM product_lines WHERE case_id = ?1 ORDER BY created_at, id",
        )?;
        let rows = statement.query_map([case_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, u32>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, String>(12)?,
                row.get::<_, String>(13)?,
                row.get::<_, String>(14)?,
                row.get::<_, String>(15)?,
                row.get::<_, String>(16)?,
                row.get::<_, String>(17)?,
                row.get::<_, bool>(18)?,
                row.get::<_, Option<String>>(19)?,
                row.get::<_, String>(20)?,
            ))
        })?;

        rows.map(|row| {
            let (
                id,
                case_id,
                name,
                seller,
                quantity,
                unit_price,
                discount,
                shipping,
                taxes,
                currency,
                _stored_subtotal_original,
                _stored_total_original,
                _stored_total_mxn,
                rate_date,
                rate_to_mxn,
                rate_source_name,
                rate_source_url,
                rate_fetched_at,
                rate_is_manual,
                manual_rate_reason,
                created_at,
            ) = row?;
            let parsed_case_id = Uuid::parse_str(&case_id)?;
            let draft = ProductDraft::new(
                name,
                seller,
                quantity,
                Decimal::from_str(&unit_price)?,
                Decimal::from_str(&discount)?,
                Decimal::from_str(&shipping)?,
                Decimal::from_str(&taxes)?,
                currency.clone(),
            )?;
            let rate = ExchangeRateSnapshot {
                currency,
                rate_date: NaiveDate::parse_from_str(&rate_date, "%Y-%m-%d")?,
                rate_to_mxn: Decimal::from_str(&rate_to_mxn)?,
                source_name: rate_source_name,
                source_url: rate_source_url,
                fetched_at: parse_datetime(&rate_fetched_at, "Fecha de tasa inválida")?,
                is_manual: rate_is_manual,
                manual_reason: manual_rate_reason,
            };
            let calculated = ProductLine::new(parsed_case_id, draft, rate)?;
            Ok(ProductLine {
                id: Uuid::parse_str(&id)?,
                created_at: parse_datetime(&created_at, "Fecha de producto inválida")?,
                ..calculated
            })
        })
        .collect()
    }

    pub fn add_product(&self, product: &ProductLine) -> Result<()> {
        self.add_products(std::slice::from_ref(product))
    }

    pub fn add_products(&self, products: &[ProductLine]) -> Result<()> {
        let Some(first_product) = products.first() else {
            bail!("Agrega al menos un producto");
        };
        if products
            .iter()
            .any(|product| product.case_id != first_product.case_id)
        {
            bail!("Todos los productos deben pertenecer al mismo expediente");
        }
        self.repository.find(first_product.case_id)?;
        let mut connection = self.repository.connection()?;
        let transaction = connection.transaction()?;
        for product in products {
            insert_product(&transaction, product)?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn remove_product(&self, product: &ProductLine) -> Result<()> {
        let mut connection = self.repository.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "DELETE FROM product_lines WHERE id = ?1 AND case_id = ?2",
            params![product.id.to_string(), product.case_id.to_string()],
        )?;
        insert_audit_event(
            &transaction,
            product.case_id,
            "product_removed",
            &format!("Producto retirado: {}", product.name),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn record_document_generation(&self, case_id: Uuid) -> Result<()> {
        self.repository.find(case_id)?;
        let mut connection = self.repository.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE rectification_cases
             SET status = 'documents_generated', updated_at = ?1
             WHERE id = ?2",
            params![Utc::now().to_rfc3339(), case_id.to_string()],
        )?;
        insert_audit_event(
            &transaction,
            case_id,
            "documents_generated",
            "Solicitud, dossier, correo y paquete ZIP generados",
        )?;
        transaction.commit()?;
        Ok(())
    }
}

fn insert_product(transaction: &Transaction<'_>, product: &ProductLine) -> Result<()> {
    transaction.execute(
        "INSERT INTO product_lines (
            id, case_id, name, seller, quantity, unit_price, discount, shipping,
            taxes, currency, subtotal_original, total_original, total_mxn,
            rate_date, rate_to_mxn, rate_source_name, rate_source_url,
            rate_fetched_at, rate_is_manual, manual_rate_reason, created_at
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
            ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21
         )",
        params![
            product.id.to_string(),
            product.case_id.to_string(),
            product.name,
            product.seller,
            product.quantity,
            product.unit_price.to_string(),
            product.discount.to_string(),
            product.shipping.to_string(),
            product.taxes.to_string(),
            product.currency,
            product.subtotal_original.to_string(),
            product.total_original.to_string(),
            product.total_mxn.to_string(),
            product.rate.rate_date.to_string(),
            product.rate.rate_to_mxn.to_string(),
            product.rate.source_name,
            product.rate.source_url,
            product.rate.fetched_at.to_rfc3339(),
            product.rate.is_manual,
            product.rate.manual_reason,
            product.created_at.to_rfc3339(),
        ],
    )?;
    insert_audit_event(
        transaction,
        product.case_id,
        "product_added",
        &format!(
            "Producto agregado: {} ({} {} = {} MXN)",
            product.name, product.total_original, product.currency, product.total_mxn
        ),
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct EvidenceVault {
    repository: SqliteCaseRepository,
    attachments_dir: PathBuf,
    cipher: AttachmentCipher,
}

impl EvidenceVault {
    pub fn open_default() -> Result<Self> {
        let project_dirs = app_project_dirs()?;
        let repository =
            SqliteCaseRepository::open(project_dirs.data_local_dir().join(DATABASE_FILENAME))?;
        let attachments_dir = project_dirs.data_local_dir().join("attachments");
        let cipher = AttachmentCipher::load_or_create_platform()
            .context("No se pudo preparar el cifrado local")?;
        Self::open(repository, attachments_dir, cipher)
    }

    pub fn open(
        repository: SqliteCaseRepository,
        attachments_dir: impl Into<PathBuf>,
        cipher: AttachmentCipher,
    ) -> Result<Self> {
        let attachments_dir = attachments_dir.into();
        fs::create_dir_all(&attachments_dir).with_context(|| {
            format!(
                "No se pudo crear el almacén cifrado {}",
                attachments_dir.display()
            )
        })?;
        Ok(Self {
            repository,
            attachments_dir,
            cipher,
        })
    }

    pub fn delete_case(&self, case_id: Uuid) -> Result<()> {
        self.repository.find(case_id)?;
        let deleted = self.repository.connection()?.execute(
            "DELETE FROM rectification_cases WHERE id = ?1",
            [case_id.to_string()],
        )?;
        if deleted != 1 {
            bail!("No se encontró el expediente que deseas eliminar");
        }

        let encrypted_documents = self.attachments_dir.join(case_id.to_string());
        if encrypted_documents.is_dir() {
            fs::remove_dir_all(&encrypted_documents).with_context(|| {
                format!(
                    "El expediente se eliminó, pero no fue posible retirar {}",
                    encrypted_documents.display()
                )
            })?;
        }

        let database_directory = self
            .repository
            .database_path
            .parent()
            .context("La ruta de la base de datos local no es válida")?;
        let generated_documents = database_directory
            .join("generated_documents")
            .join(case_id.to_string());
        if generated_documents.is_dir() {
            fs::remove_dir_all(&generated_documents).with_context(|| {
                format!(
                    "El expediente se eliminó, pero no fue posible retirar {}",
                    generated_documents.display()
                )
            })?;
        }
        Ok(())
    }

    pub fn list_evidence(&self, case_id: Uuid) -> Result<Vec<EvidenceDocument>> {
        let connection = self.repository.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, case_id, kind, title, original_filename, content_type, size_bytes,
                    sha256, encrypted_relative_path, order_index, created_at
             FROM evidence_documents WHERE case_id = ?1 ORDER BY order_index, created_at",
        )?;
        let rows = statement.query_map([case_id.to_string()], evidence_row)?;
        rows.map(|row| evidence_from_row(row?)).collect()
    }

    pub fn list_audit_events(&self, case_id: Uuid) -> Result<Vec<AuditEvent>> {
        let connection = self.repository.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, case_id, event_type, summary, created_at
             FROM audit_events WHERE case_id = ?1 ORDER BY created_at DESC",
        )?;
        let rows = statement.query_map([case_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        rows.map(|row| {
            let (id, case_id, event_type, summary, created_at) = row?;
            Ok(AuditEvent {
                id: Uuid::parse_str(&id)?,
                case_id: Uuid::parse_str(&case_id)?,
                event_type,
                summary,
                created_at: parse_datetime(&created_at, "Fecha de bitácora inválida")?,
            })
        })
        .collect()
    }

    pub fn import_evidence(
        &self,
        case_id: Uuid,
        kind: EvidenceKind,
        title: Option<String>,
        source_path: &Path,
    ) -> Result<EvidenceDocument> {
        self.repository.find(case_id)?;
        let metadata = fs::metadata(source_path)
            .with_context(|| format!("No se pudo leer {}", source_path.display()))?;
        if !metadata.is_file() {
            bail!("La evidencia seleccionada no es un archivo");
        }
        if metadata.len() > MAX_EVIDENCE_SIZE_BYTES {
            bail!("Cada evidencia puede pesar como máximo 25 MB");
        }

        let plaintext = fs::read(source_path)
            .with_context(|| format!("No se pudo leer {}", source_path.display()))?;
        let content_type = detect_content_type(&plaintext)?;
        let original_filename = source_path
            .file_name()
            .and_then(|name| name.to_str())
            .context("El nombre del archivo no es válido")?
            .to_owned();
        let id = Uuid::new_v4();
        let id_text = id.to_string();
        let encrypted = self.cipher.encrypt(&plaintext, id_text.as_bytes())?;
        let relative_path = format!("{case_id}/{id}.mrmx");
        let final_path = self.attachments_dir.join(&relative_path);
        let parent = final_path.parent().context("Ruta cifrada inválida")?;
        fs::create_dir_all(parent)?;
        let temporary_path = parent.join(format!(".{id}.encrypted-tmp"));
        write_new_file(&temporary_path, &encrypted)?;
        fs::rename(&temporary_path, &final_path)
            .with_context(|| format!("No se pudo finalizar el cifrado de {original_filename}"))?;

        let result = (|| -> Result<EvidenceDocument> {
            let mut connection = self.repository.connection()?;
            let transaction = connection.transaction()?;
            let order_index: i64 = transaction.query_row(
                "SELECT COALESCE(MAX(order_index), -1) + 1 FROM evidence_documents WHERE case_id = ?1",
                [case_id.to_string()],
                |row| row.get(0),
            )?;
            let document = EvidenceDocument {
                id,
                case_id,
                kind,
                title: title
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| kind.label().to_owned()),
                original_filename,
                content_type,
                size_bytes: metadata.len(),
                sha256: sha256_hex(&plaintext),
                encrypted_relative_path: relative_path,
                order_index,
                created_at: Utc::now(),
            };
            transaction.execute(
                "INSERT INTO evidence_documents (
                    id, case_id, kind, title, original_filename, content_type, size_bytes,
                    sha256, encrypted_relative_path, order_index, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    document.id.to_string(),
                    document.case_id.to_string(),
                    document.kind.as_str(),
                    document.title,
                    document.original_filename,
                    document.content_type,
                    document.size_bytes,
                    document.sha256,
                    document.encrypted_relative_path,
                    document.order_index,
                    document.created_at.to_rfc3339(),
                ],
            )?;
            insert_audit_event(
                &transaction,
                case_id,
                "evidence_added",
                &format!("Evidencia agregada: {}", document.title),
            )?;
            transaction.commit()?;
            Ok(document)
        })();

        if result.is_err() {
            let _ = fs::remove_file(&final_path);
        }
        result
    }

    pub fn load_evidence_bytes(&self, document: &EvidenceDocument) -> Result<Vec<u8>> {
        let encrypted_path = self.attachments_dir.join(&document.encrypted_relative_path);
        let encrypted = fs::read(&encrypted_path)
            .with_context(|| format!("No se pudo leer {}", encrypted_path.display()))?;
        let id_text = document.id.to_string();
        let plaintext = self.cipher.decrypt(&encrypted, id_text.as_bytes())?;
        if sha256_hex(&plaintext) != document.sha256 {
            bail!("La evidencia no coincide con su huella de integridad");
        }
        Ok(plaintext)
    }

    pub fn remove_evidence(&self, document: &EvidenceDocument) -> Result<()> {
        let final_path = self.attachments_dir.join(&document.encrypted_relative_path);
        let quarantine_path = final_path.with_extension("mrmx-removing");
        fs::rename(&final_path, &quarantine_path)
            .context("No se pudo preparar el retiro del archivo cifrado")?;

        let result = (|| -> Result<()> {
            let mut connection = self.repository.connection()?;
            let transaction = connection.transaction()?;
            transaction.execute(
                "DELETE FROM evidence_documents WHERE id = ?1 AND case_id = ?2",
                params![document.id.to_string(), document.case_id.to_string()],
            )?;
            insert_audit_event(
                &transaction,
                document.case_id,
                "evidence_removed",
                &format!("Evidencia retirada: {}", document.title),
            )?;
            transaction.commit()?;
            Ok(())
        })();

        match result {
            Ok(()) => {
                fs::remove_file(&quarantine_path)
                    .context("La evidencia se retiró, pero no se pudo borrar el archivo cifrado")?;
                Ok(())
            }
            Err(error) => {
                let _ = fs::rename(&quarantine_path, &final_path);
                Err(error)
            }
        }
    }

    pub fn move_evidence(&self, case_id: Uuid, document_id: Uuid, offset: isize) -> Result<()> {
        let documents = self.list_evidence(case_id)?;
        let current = documents
            .iter()
            .position(|document| document.id == document_id)
            .context("La evidencia no existe")?;
        let target = current as isize + offset;
        if target < 0 || target >= documents.len() as isize {
            return Ok(());
        }

        let target = target as usize;
        let mut connection = self.repository.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE evidence_documents SET order_index = ?1 WHERE id = ?2",
            params![
                documents[target].order_index,
                documents[current].id.to_string()
            ],
        )?;
        transaction.execute(
            "UPDATE evidence_documents SET order_index = ?1 WHERE id = ?2",
            params![
                documents[current].order_index,
                documents[target].id.to_string()
            ],
        )?;
        insert_audit_event(
            &transaction,
            case_id,
            "evidence_reordered",
            "Se modificó el orden de las evidencias",
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn export_case(&self, case_id: Uuid, destination: &Path) -> Result<PathBuf> {
        let case = self.repository.find(case_id)?;
        let documents = self.list_evidence(case_id)?;
        let export_dir = destination.join(format!(
            "MiRectificacion-{}-{}",
            case.tracking_number,
            Utc::now().format("%Y%m%d-%H%M%S")
        ));
        fs::create_dir(&export_dir)
            .with_context(|| format!("No se pudo crear {}", export_dir.display()))?;

        for (index, document) in documents.iter().enumerate() {
            let bytes = self.load_evidence_bytes(document)?;
            let filename = format!(
                "{:02}_{}",
                index + 1,
                sanitize_filename(&document.original_filename)
            );
            write_new_file(&export_dir.join(filename), &bytes)?;
        }

        let manifest = ExportManifest {
            format_version: 1,
            exported_at: Utc::now(),
            case,
            evidence: documents,
        };
        write_new_file(
            &export_dir.join("manifest.json"),
            &serde_json::to_vec_pretty(&manifest)?,
        )?;

        let mut connection = self.repository.connection()?;
        let transaction = connection.transaction()?;
        insert_audit_event(
            &transaction,
            case_id,
            "case_exported",
            "Expediente exportado por la persona usuaria",
        )?;
        transaction.commit()?;
        Ok(export_dir)
    }
}

#[derive(Serialize)]
struct ExportManifest {
    format_version: u8,
    exported_at: DateTime<Utc>,
    case: RectificationCase,
    evidence: Vec<EvidenceDocument>,
}

type EvidenceRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    u64,
    String,
    String,
    i64,
    String,
);

fn evidence_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EvidenceRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
    ))
}

fn evidence_from_row(row: EvidenceRow) -> Result<EvidenceDocument> {
    let (
        id,
        case_id,
        kind,
        title,
        original_filename,
        content_type,
        size_bytes,
        sha256,
        encrypted_relative_path,
        order_index,
        created_at,
    ) = row;
    Ok(EvidenceDocument {
        id: Uuid::parse_str(&id)?,
        case_id: Uuid::parse_str(&case_id)?,
        kind: EvidenceKind::from_str(&kind)?,
        title,
        original_filename,
        content_type,
        size_bytes,
        sha256,
        encrypted_relative_path,
        order_index,
        created_at: parse_datetime(&created_at, "Fecha de evidencia inválida")?,
    })
}

fn insert_audit_event(
    transaction: &Transaction<'_>,
    case_id: Uuid,
    event_type: &str,
    summary: &str,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO audit_events (id, case_id, event_type, summary, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            Uuid::new_v4().to_string(),
            case_id.to_string(),
            event_type,
            summary,
            Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn parse_datetime(value: &str, context: &'static str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)
        .context(context)?
        .with_timezone(&Utc))
}

fn detect_content_type(bytes: &[u8]) -> Result<String> {
    let mime = infer::get(bytes)
        .map(|kind| kind.mime_type())
        .ok_or_else(|| anyhow!("No se reconoció el tipo de archivo"))?;
    match mime {
        "application/pdf" | "image/jpeg" | "image/png" | "image/webp" => Ok(mime.to_owned()),
        _ => bail!("Sólo se permiten PDF, JPEG, PNG y WebP"),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn tracking_fingerprint(event: &TrackingEventInput) -> String {
    let occurred_at = event
        .occurred_at
        .map(|value| value.to_rfc3339())
        .unwrap_or_default();
    let description = event
        .description
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let location = event
        .location
        .as_deref()
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    sha256_hex(format!("{occurred_at}\n{location}\n{description}").as_bytes())
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("No se pudo crear {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn sanitize_filename(filename: &str) -> String {
    filename
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

#[cfg(test)]
mod tests {
    use super::*;
    use mi_rectificacion_domain::ProductDraft;

    fn test_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("mi-rectificacion-{label}-{}", Uuid::new_v4()))
    }

    #[test]
    fn persists_and_reopens_a_case() {
        let root = test_dir("case-storage");
        let database_path = root.join("data.db");
        let repository = SqliteCaseRepository::open(&database_path).unwrap();
        let case = RectificationCase::new("RR123456789MX", None, None).unwrap();
        repository.insert(&case).unwrap();
        drop(repository);

        let reopened = SqliteCaseRepository::open(&database_path).unwrap();
        assert_eq!(reopened.list().unwrap(), vec![case]);
        let audit_count = reopened
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM audit_events WHERE event_type = 'case_created'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        assert_eq!(audit_count, 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn archives_and_restores_a_case_without_changing_its_status() {
        let root = test_dir("case-archive");
        let repository = SqliteCaseRepository::open(root.join("data.db")).unwrap();
        let case = RectificationCase::new("RR123456789MX", None, None).unwrap();
        repository.insert(&case).unwrap();
        let store = CaseDataStore::open(repository.clone());

        store.set_case_archived(case.id, true).unwrap();
        let archived = repository.list().unwrap().remove(0);
        assert!(archived.archived_at.is_some());
        assert_eq!(archived.status, CaseStatus::Draft);

        store.set_case_archived(case.id, false).unwrap();
        let restored = repository.list().unwrap().remove(0);
        assert!(restored.archived_at.is_none());
        assert_eq!(restored.status, CaseStatus::Draft);

        let audit_events = EvidenceVault::open(
            repository,
            root.join("attachments"),
            AttachmentCipher::from_key([31; 32]),
        )
        .unwrap()
        .list_audit_events(case.id)
        .unwrap();
        assert!(
            audit_events
                .iter()
                .any(|event| event.event_type == "case_archived")
        );
        assert!(
            audit_events
                .iter()
                .any(|event| event.event_type == "case_restored")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resumes_and_completes_the_guided_capture() {
        let root = test_dir("capture-progress");
        let repository = SqliteCaseRepository::open(root.join("data.db")).unwrap();
        let case = RectificationCase::new("RR123456789MX", None, None).unwrap();
        repository.insert(&case).unwrap();
        let store = CaseDataStore::open(repository);

        store.save_capture_progress(case.id, 2).unwrap();
        assert_eq!(store.incomplete_capture().unwrap(), Some((case.id, 2)));
        store.save_capture_progress(case.id, 4).unwrap();
        assert_eq!(store.incomplete_capture().unwrap(), Some((case.id, 4)));
        store.complete_capture(case.id).unwrap();
        assert_eq!(store.incomplete_capture().unwrap(), None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn encrypts_previews_exports_and_removes_evidence() {
        let root = test_dir("evidence-vault");
        let repository = SqliteCaseRepository::open(root.join("data.db")).unwrap();
        let case = RectificationCase::new("RR123456789MX", None, None).unwrap();
        repository.insert(&case).unwrap();
        let vault = EvidenceVault::open(
            repository,
            root.join("attachments"),
            AttachmentCipher::from_key([42; 32]),
        )
        .unwrap();

        let source = root.join("proof.pdf");
        let pdf = include_bytes!("../../../tests/fixtures/minimal.pdf");
        fs::create_dir_all(&root).unwrap();
        fs::write(&source, pdf).unwrap();
        let document = vault
            .import_evidence(
                case.id,
                EvidenceKind::Transaction,
                Some("Pago confirmado".to_owned()),
                &source,
            )
            .unwrap();
        assert_eq!(document.content_type, "application/pdf");

        let encrypted = fs::read(
            root.join("attachments")
                .join(&document.encrypted_relative_path),
        )
        .unwrap();
        assert!(!encrypted.windows(pdf.len()).any(|window| window == pdf));
        assert_eq!(vault.load_evidence_bytes(&document).unwrap(), pdf);
        assert_eq!(
            vault.list_evidence(case.id).unwrap(),
            vec![document.clone()]
        );

        let second = vault
            .import_evidence(
                case.id,
                EvidenceKind::BankStatement,
                Some("Estado de cuenta".to_owned()),
                &source,
            )
            .unwrap();
        assert_eq!(second.content_type, "application/pdf");
        vault.move_evidence(case.id, second.id, -1).unwrap();
        assert_eq!(vault.list_evidence(case.id).unwrap()[0].id, second.id);

        let export_root = root.join("exports");
        fs::create_dir_all(&export_root).unwrap();
        let exported = vault.export_case(case.id, &export_root).unwrap();
        assert_eq!(fs::read(exported.join("01_proof.pdf")).unwrap(), pdf);
        assert_eq!(fs::read(exported.join("02_proof.pdf")).unwrap(), pdf);
        assert!(exported.join("manifest.json").is_file());

        vault.remove_evidence(&document).unwrap();
        let remaining = vault.list_evidence(case.id).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, second.id);
        assert!(
            !root
                .join("attachments")
                .join(document.encrypted_relative_path)
                .exists()
        );
        vault.remove_evidence(&second).unwrap();
        assert!(vault.list_evidence(case.id).unwrap().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn persists_exact_product_amounts_and_rate_snapshot() {
        let root = test_dir("products");
        let repository = SqliteCaseRepository::open(root.join("data.db")).unwrap();
        let case = RectificationCase::new("RR123456789MX", None, None).unwrap();
        repository.insert(&case).unwrap();
        let store = CaseDataStore::open(repository);
        let draft = ProductDraft::new(
            "Consola",
            Some("Tienda".to_owned()),
            1,
            Decimal::from_str("32500.50").unwrap(),
            Decimal::from_str("500.25").unwrap(),
            Decimal::from_str("1200").unwrap(),
            Decimal::ZERO,
            "JPY",
        )
        .unwrap();
        let rate = ExchangeRateSnapshot::automatic(
            "JPY",
            NaiveDate::from_ymd_opt(2026, 8, 14).unwrap(),
            Decimal::from_str("0.10688").unwrap(),
            "Proveedor de prueba",
            "https://example.test/rate",
            Utc::now(),
        )
        .unwrap();
        let product = ProductLine::new(case.id, draft, rate).unwrap();
        store.add_product(&product).unwrap();

        store
            .repository
            .connection()
            .unwrap()
            .execute(
                "UPDATE product_lines
                 SET subtotal_original = '1', total_original = '999999', total_mxn = '999999'
                 WHERE id = ?1",
                [product.id.to_string()],
            )
            .unwrap();

        assert_eq!(store.list_products(case.id).unwrap(), vec![product.clone()]);
        store.remove_product(&product).unwrap();
        assert!(store.list_products(case.id).unwrap().is_empty());
        let events = EvidenceVault::open(
            store.repository.clone(),
            root.join("attachments"),
            AttachmentCipher::from_key([11; 32]),
        )
        .unwrap()
        .list_audit_events(case.id)
        .unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "product_added")
        );
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "product_removed")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn adds_multiple_products_in_one_batch() {
        let root = test_dir("product-batch");
        let repository = SqliteCaseRepository::open(root.join("data.db")).unwrap();
        let case = RectificationCase::new("RR123456789MX", None, None).unwrap();
        repository.insert(&case).unwrap();
        let store = CaseDataStore::open(repository);
        let rate = ExchangeRateSnapshot::automatic(
            "JPY",
            NaiveDate::from_ymd_opt(2026, 8, 15).unwrap(),
            Decimal::from_str("0.12").unwrap(),
            "Proveedor de prueba",
            "https://example.test/rate",
            Utc::now(),
        )
        .unwrap();
        let make_product = |name: &str, price: i64| {
            ProductLine::new(
                case.id,
                ProductDraft::new(
                    name,
                    None,
                    1,
                    Decimal::from(price),
                    Decimal::ZERO,
                    Decimal::ZERO,
                    Decimal::ZERO,
                    "JPY",
                )
                .unwrap(),
                rate.clone(),
            )
            .unwrap()
        };
        let products = vec![
            make_product("Artículo uno", 1000),
            make_product("Artículo dos", 2000),
        ];

        store.add_products(&products).unwrap();
        let saved = store.list_products(case.id).unwrap();
        assert_eq!(saved.len(), 2);
        assert!(saved.iter().any(|product| product.name == "Artículo uno"));
        assert!(saved.iter().any(|product| product.name == "Artículo dos"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn deletes_only_the_selected_case_and_its_local_files() {
        let root = test_dir("delete-case");
        let repository = SqliteCaseRepository::open(root.join("data.db")).unwrap();
        let deleted_case = RectificationCase::new("RR123456789MX", None, None).unwrap();
        let retained_case = RectificationCase::new("ZZ000000000ZZ", None, None).unwrap();
        repository.insert(&deleted_case).unwrap();
        repository.insert(&retained_case).unwrap();
        let vault = EvidenceVault::open(
            repository.clone(),
            root.join("attachments"),
            AttachmentCipher::from_key([24; 32]),
        )
        .unwrap();
        let source = root.join("proof.pdf");
        fs::write(
            &source,
            include_bytes!("../../../tests/fixtures/minimal.pdf"),
        )
        .unwrap();
        let deleted_document = vault
            .import_evidence(
                deleted_case.id,
                EvidenceKind::Product,
                Some("Factura".to_owned()),
                &source,
            )
            .unwrap();
        let retained_document = vault
            .import_evidence(
                retained_case.id,
                EvidenceKind::BankStatement,
                Some("Estado de cuenta".to_owned()),
                &source,
            )
            .unwrap();
        let store = CaseDataStore::open(repository.clone());
        let generated = store
            .automatic_email_documents_root(deleted_case.id)
            .unwrap();
        fs::write(generated.join("fixture.txt"), b"generated").unwrap();

        vault.delete_case(deleted_case.id).unwrap();

        assert_eq!(repository.list().unwrap(), vec![retained_case]);
        assert!(
            !root
                .join("attachments")
                .join(deleted_case.id.to_string())
                .exists()
        );
        assert!(!generated.exists());
        assert_eq!(
            vault.load_evidence_bytes(&retained_document).unwrap(),
            include_bytes!("../../../tests/fixtures/minimal.pdf")
        );
        assert!(vault.load_evidence_bytes(&deleted_document).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn deduplicates_tracking_events_and_creates_one_unseen_update() {
        let root = test_dir("tracking");
        let repository = SqliteCaseRepository::open(root.join("data.db")).unwrap();
        let case = RectificationCase::new("ZZ000000000ZZ", None, None).unwrap();
        repository.insert(&case).unwrap();
        let store = CaseDataStore::open(repository.clone());
        let event = TrackingEventInput {
            occurred_at: Some(
                DateTime::parse_from_rfc3339("2026-08-14T12:30:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            description: "En tránsito hacia destino".to_owned(),
            location: Some("Centro Operativo".to_owned()),
        };

        let first = store
            .record_tracking_response(
                case.id,
                "correos_mexico",
                "<html>primera respuesta</html>",
                Utc::now(),
                std::slice::from_ref(&event),
            )
            .unwrap();
        let second = store
            .record_tracking_response(
                case.id,
                "correos_mexico",
                "<html>respuesta repetida</html>",
                Utc::now(),
                &[event],
            )
            .unwrap();

        assert_eq!(first.inserted_events, 1);
        assert_eq!(second.inserted_events, 0);
        assert_eq!(second.total_events, 1);
        assert_eq!(store.list_tracking_events(case.id).unwrap().len(), 1);
        assert!(repository.list().unwrap()[0].has_unseen_updates);
        let tracking_audits = EvidenceVault::open(
            repository.clone(),
            root.join("attachments"),
            AttachmentCipher::from_key([13; 32]),
        )
        .unwrap()
        .list_audit_events(case.id)
        .unwrap()
        .into_iter()
        .filter(|event| event.event_type == "tracking_updated")
        .count();
        assert_eq!(tracking_audits, 1);
        let snapshots = repository
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM tracking_snapshots", [], |row| {
                row.get::<_, usize>(0)
            })
            .unwrap();
        assert_eq!(snapshots, 2);

        store.mark_tracking_seen(case.id).unwrap();
        assert!(!repository.list().unwrap()[0].has_unseen_updates);
        assert!(store.list_tracking_events(case.id).unwrap()[0].is_seen);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn persists_the_single_local_applicant_profile() {
        let root = test_dir("applicant-profile");
        let repository = SqliteCaseRepository::open(root.join("data.db")).unwrap();
        let store = CaseDataStore::open(repository);
        assert_eq!(
            store.load_applicant_profile().unwrap(),
            ApplicantProfile::default()
        );

        let profile = ApplicantProfile {
            full_name: "  María López  ".to_owned(),
            email: " maria@example.test ".to_owned(),
            phone: "644 123 4567".to_owned(),
            address: "Calle Principal 123".to_owned(),
            city: "Ciudad Obregón".to_owned(),
            state: "Sonora".to_owned(),
            postal_code: "85000".to_owned(),
        };
        store.save_applicant_profile(&profile).unwrap();

        let saved = store.load_applicant_profile().unwrap();
        assert_eq!(saved.full_name, "María López");
        assert_eq!(saved.email, "maria@example.test");
        assert_eq!(saved.phone, profile.phone);
        assert_eq!(saved.address, profile.address);
        assert_eq!(saved.city, profile.city);
        assert_eq!(saved.state, profile.state);
        assert_eq!(saved.postal_code, profile.postal_code);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn persists_the_customs_valuation_for_each_case() {
        let root = test_dir("customs-valuation");
        let repository = SqliteCaseRepository::open(root.join("data.db")).unwrap();
        let case = RectificationCase::new("RR123456789MX", None, None).unwrap();
        repository.insert(&case).unwrap();
        let store = CaseDataStore::open(repository);

        assert_eq!(store.load_customs_valuation(case.id).unwrap(), None);
        store
            .save_customs_valuation(case.id, Decimal::new(605_587, 2))
            .unwrap();
        assert_eq!(
            store.load_customs_valuation(case.id).unwrap(),
            Some(Decimal::new(605_587, 2))
        );
        assert!(
            store
                .save_customs_valuation(case.id, Decimal::ZERO)
                .is_err()
        );
        let email_documents = store.automatic_email_documents_root(case.id).unwrap();
        assert!(email_documents.is_dir());
        assert!(email_documents.starts_with(&root));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn completes_onboarding_and_profile_atomically() {
        let root = test_dir("onboarding");
        let repository = SqliteCaseRepository::open(root.join("data.db")).unwrap();
        let store = CaseDataStore::open(repository);
        assert!(!store.is_onboarding_completed().unwrap());

        let profile = ApplicantProfile {
            full_name: "  Ana Pérez  ".to_owned(),
            email: " ana@example.test ".to_owned(),
            phone: "644 000 0000".to_owned(),
            address: "Calle Uno 10".to_owned(),
            city: "Hermosillo".to_owned(),
            state: "Sonora".to_owned(),
            postal_code: "83000".to_owned(),
        };
        store.complete_onboarding(&profile).unwrap();

        assert!(store.is_onboarding_completed().unwrap());
        let saved = store.load_applicant_profile().unwrap();
        assert_eq!(saved.full_name, "Ana Pérez");
        assert_eq!(saved.email, "ana@example.test");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn persists_email_preparation_opening_and_manual_sent_status() {
        let root = test_dir("email-draft");
        let repository = SqliteCaseRepository::open(root.join("data.db")).unwrap();
        let case = RectificationCase::new("RR123456789MX", None, None).unwrap();
        repository.insert(&case).unwrap();
        let store = CaseDataStore::open(repository.clone());
        assert!(store.load_email_draft(case.id).unwrap().is_none());

        let prepared_at = Utc::now();
        let draft = EmailDraft {
            case_id: case.id,
            recipient: "aduana@example.gob.mx".to_owned(),
            sender: "persona@example.com".to_owned(),
            subject: "Solicitud de rectificación".to_owned(),
            body: "Adjunto los documentos para su revisión.".to_owned(),
            request_pdf_path: "/tmp/solicitud.pdf".to_owned(),
            evidence_pdf_path: "/tmp/dossier.pdf".to_owned(),
            eml_path: "/tmp/borrador.eml".to_owned(),
            prepared_at,
            opened_at: None,
            sent_at: None,
        };
        store.save_email_draft(&draft).unwrap();
        assert_eq!(
            repository.list().unwrap()[0].status,
            CaseStatus::EmailPrepared
        );
        assert_eq!(store.load_email_draft(case.id).unwrap(), Some(draft));

        store.record_email_opened(case.id).unwrap();
        assert!(
            store
                .load_email_draft(case.id)
                .unwrap()
                .unwrap()
                .opened_at
                .is_some()
        );
        store.mark_email_sent(case.id).unwrap();
        assert_eq!(repository.list().unwrap()[0].status, CaseStatus::Sent);
        assert!(
            store
                .load_email_draft(case.id)
                .unwrap()
                .unwrap()
                .sent_at
                .is_some()
        );
        let _ = fs::remove_dir_all(root);
    }
}
