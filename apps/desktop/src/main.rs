use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime, Utc};
use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
use dioxus::prelude::*;
use mi_rectificacion_application::{
    ApplicationError, CaseRepository, CreateCaseInput, create_case,
};
use mi_rectificacion_documents::{
    ApplicantDetails, EmailContent, EvidenceAsset, GeneratedBundle, export_editable_docx,
    export_print_ready_pdf, generate_bundle, write_email_draft,
};
use mi_rectificacion_domain::{
    ApplicantProfile, AuditEvent, EmailDraft, EvidenceDocument, EvidenceKind, ExchangeRateSnapshot,
    ProductDraft, ProductLine, RectificationCase, TrackingEvent, TrackingEventInput,
    TrackingRefreshState, calculate_customs_overvaluation,
};
use mi_rectificacion_integrations::{
    CorreosMexicoProvider, ExchangeRateProvider, FrankfurterProvider, TrackingProvider,
};
use mi_rectificacion_storage::{
    CaseDataStore, EvidenceVault, SqliteCaseRepository, TrackingUpdateResult,
};
use rust_decimal::Decimal;
use std::{path::Path, str::FromStr, time::Duration};
use uuid::Uuid;

const CSS: &str = include_str!("../../../assets/main.css");
const LOGO_PNG: &[u8] = include_bytes!("../../../assets/mi-rectificacion-mx-logo.png");
const APP_ICON_PNG: &[u8] = include_bytes!("../../../assets/icons/icon.png");
const DEFAULT_CUSTOMS_EMAIL: &str = "cdabjtramites@correosdemexico.gob.mx";
const FACEBOOK_URL: &str = "https://www.facebook.com/share/g/19DFq8Qp6d/";
const GITHUB_URL: &str = "https://github.com/richtunic/mi-rectificacion-mx";
const KOFI_URL: &str = "https://ko-fi.com/relampagonegr0";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const TRACKING_REFRESH_INTERVAL_HOURS: i64 = 12;
const TRACKING_SCHEDULER_POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);
const SHOW_EMAIL_WORKFLOW: bool = false;

fn logo_data_uri() -> String {
    format!("data:image/png;base64,{}", STANDARD.encode(LOGO_PNG))
}

#[cfg(target_os = "macos")]
fn set_macos_application_icon() {
    use objc2::{AllocAnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    let main_thread = MainThreadMarker::new()
        .expect("El icono de macOS debe configurarse desde el hilo principal");
    let application = NSApplication::sharedApplication(main_thread);
    let data = NSData::with_bytes(APP_ICON_PNG);
    let icon = NSImage::initWithData(NSImage::alloc(), &data)
        .expect("El icono PNG incluido debe ser válido para macOS");
    unsafe { application.setApplicationIconImage(Some(&icon)) };
}

fn tracking_source_label(source: &str) -> &str {
    match source {
        "correos_mexico" => "Correos de México",
        "manual" => "Registro manual",
        value => value,
    }
}

fn current_date_in_spanish() -> String {
    let today = Local::now();
    let month = [
        "enero",
        "febrero",
        "marzo",
        "abril",
        "mayo",
        "junio",
        "julio",
        "agosto",
        "septiembre",
        "octubre",
        "noviembre",
        "diciembre",
    ][today.month0() as usize];
    format!("{} de {} de {}", today.day(), month, today.year())
}

fn main() {
    let config = Config::new()
        .with_background_color((18, 19, 17, 255))
        .with_icon(
            dioxus::desktop::icon_from_memory(APP_ICON_PNG)
                .expect("El icono PNG incluido debe ser válido"),
        )
        .with_custom_head(format!("<style>{CSS}</style>"))
        .with_window(
            WindowBuilder::new()
                .with_title("Mi Rectificación MX")
                .with_transparent(false)
                .with_inner_size(LogicalSize::new(1_200.0, 780.0))
                .with_min_inner_size(LogicalSize::new(800.0, 620.0)),
        );

    #[cfg(target_os = "macos")]
    let config = {
        let mut dock_icon_configured = false;
        config.with_custom_event_handler(move |event, _event_loop| {
            use dioxus::desktop::tao::event::Event;

            if !dock_icon_configured && matches!(event, Event::MainEventsCleared) {
                set_macos_application_icon();
                dock_icon_configured = true;
            }
        })
    };

    dioxus::LaunchBuilder::desktop()
        .with_cfg(config)
        .launch(App);
}

#[derive(Clone)]
struct AppState {
    repository: Option<SqliteCaseRepository>,
    cases: Vec<RectificationCase>,
    selected_case_id: Option<String>,
    applicant_profile: ApplicantProfile,
    show_settings: bool,
    show_faq: bool,
    show_archived_cases: bool,
    capture_case_id: Option<String>,
    capture_step: usize,
    show_onboarding: bool,
    startup_error: Option<String>,
}

impl AppState {
    fn load() -> Self {
        match SqliteCaseRepository::open_default() {
            Ok(repository) => match repository.list() {
                Ok(cases) => {
                    let store = CaseDataStore::open(repository.clone());
                    let mut startup_errors = Vec::new();
                    let incomplete_capture = store.incomplete_capture().unwrap_or_else(|error| {
                        startup_errors.push(error.to_string());
                        None
                    });
                    let capture_case_id = incomplete_capture
                        .as_ref()
                        .map(|(case_id, _)| case_id.to_string());
                    let capture_step = incomplete_capture.map(|(_, step)| step).unwrap_or(0);
                    let selected_case_id = capture_case_id
                        .clone()
                        .or_else(|| cases.first().map(|case| case.id.to_string()));
                    let applicant_profile =
                        store.load_applicant_profile().unwrap_or_else(|error| {
                            startup_errors.push(error.to_string());
                            ApplicantProfile::default()
                        });
                    let show_onboarding = store
                        .is_onboarding_completed()
                        .map(|completed| !completed)
                        .unwrap_or_else(|error| {
                            startup_errors.push(error.to_string());
                            true
                        });
                    Self {
                        repository: Some(repository),
                        cases,
                        selected_case_id,
                        applicant_profile,
                        show_settings: false,
                        show_faq: false,
                        show_archived_cases: false,
                        capture_case_id,
                        capture_step,
                        show_onboarding,
                        startup_error: (!startup_errors.is_empty())
                            .then(|| startup_errors.join(" · ")),
                    }
                }
                Err(error) => Self {
                    repository: Some(repository),
                    cases: Vec::new(),
                    selected_case_id: None,
                    applicant_profile: ApplicantProfile::default(),
                    show_settings: false,
                    show_faq: false,
                    show_archived_cases: false,
                    capture_case_id: None,
                    capture_step: 0,
                    show_onboarding: false,
                    startup_error: Some(error.to_string()),
                },
            },
            Err(error) => Self {
                repository: None,
                cases: Vec::new(),
                selected_case_id: None,
                applicant_profile: ApplicantProfile::default(),
                show_settings: false,
                show_faq: false,
                show_archived_cases: false,
                capture_case_id: None,
                capture_step: 0,
                show_onboarding: false,
                startup_error: Some(error.to_string()),
            },
        }
    }
}

#[component]
fn App() -> Element {
    let mut app_state = use_signal(AppState::load);
    let mut tracking_number = use_signal(String::new);
    let mut customs_form_number = use_signal(String::new);
    let mut display_name = use_signal(String::new);
    let mut case_search = use_signal(String::new);
    let mut new_case_step = use_signal(|| 0_usize);
    let mut form_error = use_signal(|| None::<String>);

    use_future(move || async move {
        tokio::time::sleep(Duration::from_millis(750)).await;
        loop {
            let cases = app_state.read().cases.clone();
            for case in cases.into_iter().filter(|case| case.archived_at.is_none()) {
                let tracking_number = case.tracking_number.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    if automatic_tracking_refresh_due_for_case(case.id)? {
                        refresh_tracking_case(case.id, &tracking_number).map(Some)
                    } else {
                        Ok(None)
                    }
                })
                .await;
            }
            let refreshed = app_state
                .read()
                .repository
                .as_ref()
                .and_then(|repository| repository.list().ok());
            if let Some(cases) = refreshed {
                app_state.write().cases = cases;
            }
            tokio::time::sleep(TRACKING_SCHEDULER_POLL_INTERVAL).await;
        }
    });

    let state_snapshot = app_state.read().clone();
    let wizard_step = *new_case_step.read();
    let selected_case = state_snapshot
        .selected_case_id
        .as_ref()
        .and_then(|selected_id| {
            state_snapshot
                .cases
                .iter()
                .find(|case| case.id.to_string() == *selected_id)
                .cloned()
        });
    let search_term = case_search.read().trim().to_lowercase();
    let archived_case_count = state_snapshot
        .cases
        .iter()
        .filter(|case| case.archived_at.is_some())
        .count();
    let active_case_count = state_snapshot.cases.len() - archived_case_count;
    let visible_cases = state_snapshot
        .cases
        .iter()
        .filter(|case| case.archived_at.is_some() == state_snapshot.show_archived_cases)
        .filter(|case| {
            search_term.is_empty()
                || case.display_name.to_lowercase().contains(&search_term)
                || case.tracking_number.to_lowercase().contains(&search_term)
                || case
                    .customs_form_number
                    .as_deref()
                    .is_some_and(|folio| folio.to_lowercase().contains(&search_term))
        })
        .collect::<Vec<_>>();
    let (topbar_eyebrow, topbar_title, topbar_chip) = if state_snapshot.show_faq {
        (
            "CENTRO DE AYUDA",
            "Preguntas frecuentes",
            "Orientación práctica",
        )
    } else if state_snapshot.show_settings {
        ("PREFERENCIAS LOCALES", "Configuración", "Perfil local")
    } else if selected_case.is_some() {
        (
            "EXPEDIENTE EN CURSO",
            "Centro de rectificaciones",
            "Revisión humana antes de enviar",
        )
    } else {
        ("CAPTURA INICIAL", "Nueva rectificación", "Guardado local")
    };

    rsx! {
        div { class: "app-shell",
            aside { class: "sidebar",
                div { class: "brand-row",
                    img {
                        class: "brand-logo",
                        src: logo_data_uri(),
                        alt: "Mi Rectificación MX",
                    }
                }

                button {
                    class: "new-case-button",
                    onclick: move |_| {
                        app_state.write().selected_case_id = None;
                        app_state.write().show_settings = false;
                        app_state.write().show_faq = false;
                        app_state.write().show_archived_cases = false;
                        app_state.write().capture_case_id = None;
                        app_state.write().capture_step = 0;
                        tracking_number.set(String::new());
                        customs_form_number.set(String::new());
                        display_name.set(String::new());
                        new_case_step.set(0);
                        form_error.set(None);
                    },
                    span { class: "button-icon", "+" }
                    "Nueva rectificación"
                }

                div { class: "case-search",
                    span { "⌕" }
                    input {
                        value: "{case_search}",
                        placeholder: "Buscar guía, folio o nombre",
                        aria_label: "Buscar rectificaciones",
                        oninput: move |event| case_search.set(event.value()),
                    }
                    if !case_search.read().is_empty() {
                        button {
                            r#type: "button",
                            title: "Limpiar búsqueda",
                            onclick: move |_| case_search.set(String::new()),
                            "×"
                        }
                    }
                }

                div { class: "sidebar-section-heading",
                    span { class: "sidebar-section-label",
                        if state_snapshot.show_archived_cases { "Archivadas" } else { "Rectificaciones" }
                    }
                    button {
                        class: "archive-filter-button",
                        title: if state_snapshot.show_archived_cases { "Ver rectificaciones activas" } else { "Ver rectificaciones archivadas" },
                        onclick: move |_| {
                            let mut state = app_state.write();
                            state.show_archived_cases = !state.show_archived_cases;
                            state.selected_case_id = None;
                            state.show_settings = false;
                            state.show_faq = false;
                        },
                        if state_snapshot.show_archived_cases {
                            "Activas ({active_case_count})"
                        } else {
                            "Archivadas ({archived_case_count})"
                        }
                    }
                }
                nav { class: "case-list",
                    if visible_cases.is_empty() {
                        div { class: "empty-list",
                            if search_term.is_empty() {
                                if state_snapshot.show_archived_cases {
                                    p { "No hay rectificaciones archivadas" }
                                    span { "Las que archives aparecerán aquí." }
                                } else {
                                    p { "No hay rectificaciones" }
                                    span { "Crea la primera desde el formulario." }
                                }
                            } else {
                                p { "Sin resultados" }
                                span { "Prueba con otra guía, folio o nombre." }
                            }
                        }
                    }
                    for case in visible_cases {
                        {
                            let case_id = case.id.to_string();
                            let is_selected = !state_snapshot.show_settings
                                && !state_snapshot.show_faq
                                && state_snapshot.selected_case_id.as_ref() == Some(&case_id);
                            rsx! {
                                button {
                                    key: "{case.id}",
                                    class: if is_selected { "case-row selected" } else { "case-row" },
                                    onclick: move |_| {
                                        app_state.write().selected_case_id = Some(case_id.clone());
                                        app_state.write().show_settings = false;
                                        app_state.write().show_faq = false;
                                    },
                                    div { class: "case-row-top",
                                        strong { "{case.display_name}" }
                                        if case.has_unseen_updates {
                                            span { class: "update-dot", title: "Actualización sin revisar" }
                                        }
                                    }
                                    span { class: "tracking-label", "{case.tracking_number}" }
                                    span { class: "status-label",
                                        if case.archived_at.is_some() {
                                            "Archivada · {case.status.label()}"
                                        } else {
                                            "{case.status.label()}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                div { class: "sidebar-bottom",
                    button {
                        class: if state_snapshot.show_faq { "settings-button selected" } else { "settings-button" },
                        onclick: move |_| {
                            app_state.write().show_faq = true;
                            app_state.write().show_settings = false;
                        },
                        span { class: "settings-icon", "?" }
                        div {
                            strong { "Preguntas frecuentes" }
                            span { "Ayuda para la rectificación" }
                        }
                    }
                    button {
                        class: if state_snapshot.show_settings { "settings-button selected" } else { "settings-button" },
                        onclick: move |_| {
                            app_state.write().show_settings = true;
                            app_state.write().show_faq = false;
                        },
                        span { class: "settings-icon", "⚙" }
                        div {
                            strong { "Configuración" }
                            span { "Datos del solicitante" }
                        }
                    }
                    div { class: "sidebar-footer",
                        span { class: "version-dot" }
                        div {
                            strong { "Versión {APP_VERSION}" }
                        }
                    }
                }
            }

            main { class: "content",
                header { class: "topbar",
                    div {
                        span { class: "eyebrow", "{topbar_eyebrow}" }
                        h2 { "{topbar_title}" }
                    }
                    div { class: "context-chip", "{topbar_chip}" }
                }

                if let Some(error) = state_snapshot.startup_error.as_ref() {
                    div { class: "alert error", "No se pudo abrir el almacenamiento local: {error}" }
                }

                if state_snapshot.show_faq {
                    FaqPage {}
                } else if state_snapshot.show_settings {
                    SettingsPage {
                        profile: state_snapshot.applicant_profile.clone(),
                        on_saved: move |profile| app_state.write().applicant_profile = profile,
                    }
                } else if let Some(case) = selected_case {
                    if state_snapshot.capture_case_id.as_deref() == Some(&case.id.to_string()) {
                        CaptureCaseWizard {
                            key: "capture-{case.id}",
                            case: case.clone(),
                            applicant_profile: state_snapshot.applicant_profile.clone(),
                            step: state_snapshot.capture_step,
                            on_step_changed: move |step| {
                                if let Err(error) = CaseDataStore::open_default()
                                    .and_then(|store| store.save_capture_progress(case.id, step))
                                {
                                    app_state.write().startup_error = Some(error.to_string());
                                    return;
                                }
                                app_state.write().capture_step = step;
                            },
                            on_finished: move |_| {
                                if let Err(error) = CaseDataStore::open_default()
                                    .and_then(|store| store.complete_capture(case.id))
                                {
                                    app_state.write().startup_error = Some(error.to_string());
                                    return;
                                }
                                let mut state = app_state.write();
                                state.capture_case_id = None;
                                state.capture_step = 0;
                            },
                            on_case_changed: move |_| {
                                let refreshed = app_state
                                    .read()
                                    .repository
                                    .as_ref()
                                    .and_then(|repository| repository.list().ok());
                                if let Some(cases) = refreshed {
                                    app_state.write().cases = cases;
                                }
                            },
                        }
                    } else {
                        CaseDetail {
                            key: "{case.id}",
                            case,
                            applicant_profile: state_snapshot.applicant_profile.clone(),
                            on_case_changed: move |_| {
                                let refreshed = app_state
                                    .read()
                                    .repository
                                    .as_ref()
                                    .and_then(|repository| repository.list().ok());
                                if let Some(cases) = refreshed {
                                    app_state.write().cases = cases;
                                }
                            },
                            on_archive_changed: move |archived| {
                                let refreshed = app_state
                                    .read()
                                    .repository
                                    .as_ref()
                                    .and_then(|repository| repository.list().ok());
                                if let Some(cases) = refreshed {
                                    let mut state = app_state.write();
                                    state.cases = cases;
                                    state.show_archived_cases = archived;
                                }
                            },
                            on_deleted: move |_| {
                                let refreshed = app_state
                                    .read()
                                    .repository
                                    .as_ref()
                                    .and_then(|repository| repository.list().ok());
                                if let Some(cases) = refreshed {
                                    let mut state = app_state.write();
                                    state.cases = cases;
                                    let show_archived_cases = state.show_archived_cases;
                                    state.selected_case_id = state
                                        .cases
                                        .iter()
                                        .find(|case| {
                                            case.archived_at.is_some() == show_archived_cases
                                        })
                                        .map(|case| case.id.to_string());
                                    state.capture_case_id = None;
                                    state.capture_step = 0;
                                }
                            },
                        }
                    }
                } else {
                    section { class: "welcome-panel",
                        div { class: "hero-copy",
                            span { class: "hero-icon", "↗" }
                            h3 { "Inicia una nueva rectificación" }
                            p { "Completa cada paso con calma. Antes de guardar podrás revisar los datos y después tendrás acceso a la vista completa para editarlos." }
                        }

                        form {
                            class: "new-case-form wizard-form",
                            onsubmit: move |event| {
                                event.prevent_default();
                                form_error.set(None);

                                let current_step = *new_case_step.read();
                                if current_step == 0 {
                                    let profile = app_state.read().applicant_profile.clone();
                                    if let Err(error) = validate_applicant_profile(&profile) {
                                        form_error.set(Some(error.to_owned()));
                                        return;
                                    }
                                    if let Err(error) = CaseDataStore::open_default()
                                        .and_then(|store| store.save_applicant_profile(&profile))
                                    {
                                        form_error.set(Some(error.to_string()));
                                        return;
                                    }
                                    new_case_step.set(1);
                                    return;
                                }
                                if let Err(error) = validate_wizard_tracking_number(&tracking_number.read()) {
                                    form_error.set(Some(error));
                                    return;
                                }

                                let state = app_state.read();
                                let Some(repository) = state.repository.as_ref() else {
                                    form_error.set(Some("El almacenamiento local no está disponible".to_owned()));
                                    return;
                                };

                                let input = CreateCaseInput {
                                    display_name: Some(display_name.read().clone()),
                                    tracking_number: tracking_number.read().clone(),
                                    customs_form_number: Some(customs_form_number.read().clone()),
                                };

                                match create_case(repository, input) {
                                    Ok(new_case) => {
                                        let new_case_id = new_case.id.to_string();
                                        let capture_error = CaseDataStore::open(repository.clone())
                                            .save_capture_progress(new_case.id, 2)
                                            .err()
                                            .map(|error| error.to_string());
                                        drop(state);
                                        let mut state = app_state.write();
                                        state.cases.insert(0, new_case);
                                        state.selected_case_id = Some(new_case_id);
                                        state.capture_case_id = state.selected_case_id.clone();
                                        state.capture_step = 2;
                                        if capture_error.is_some() {
                                            state.startup_error = capture_error;
                                        }
                                        tracking_number.set(String::new());
                                        customs_form_number.set(String::new());
                                        display_name.set(String::new());
                                        new_case_step.set(0);
                                    }
                                    Err(ApplicationError::DuplicateTracking { case_id, .. }) => {
                                        let existing_archived = state
                                            .cases
                                            .iter()
                                            .find(|case| case.id == case_id)
                                            .is_some_and(|case| case.archived_at.is_some());
                                        let pending_capture = CaseDataStore::open(repository.clone())
                                            .incomplete_capture()
                                            .ok()
                                            .flatten()
                                            .filter(|(pending_id, _)| *pending_id == case_id);

                                        drop(state);
                                        let mut state = app_state.write();
                                        state.selected_case_id = Some(case_id.to_string());
                                        state.show_archived_cases = existing_archived;
                                        state.show_settings = false;
                                        state.show_faq = false;

                                        if let Some((pending_id, step)) = pending_capture {
                                            state.capture_case_id = Some(pending_id.to_string());
                                            state.capture_step = step;
                                        } else {
                                            state.capture_case_id = None;
                                            state.capture_step = 0;
                                        }

                                        tracking_number.set(String::new());
                                        customs_form_number.set(String::new());
                                        display_name.set(String::new());
                                        new_case_step.set(0);
                                    }
                                    Err(error) => form_error.set(Some(error.to_string())),
                                }
                            },

                            div { class: "wizard-progress capture-start-progress",
                                button {
                                    class: if wizard_step == 0 { "active" } else { "done" },
                                    r#type: "button",
                                    onclick: move |_| {
                                        new_case_step.set(0);
                                        form_error.set(None);
                                    },
                                    span { if wizard_step > 0 { "✓" } else { "1" } }
                                    div { strong { "General" } small { "Solicitante" } }
                                }
                                button {
                                    class: if wizard_step == 1 { "active" } else { "" },
                                    r#type: "button",
                                    disabled: wizard_step < 1,
                                    onclick: move |_| {
                                        new_case_step.set(1);
                                        form_error.set(None);
                                    },
                                    span { "2" }
                                    div { strong { "Envío" } small { "Guía y boleta" } }
                                }
                                for (number, label) in [("3", "Productos"), ("4", "Comprobantes"), ("5", "Pagos"), ("6", "Revisión")] {
                                    button { r#type: "button", disabled: true,
                                        span { "{number}" }
                                        div { strong { "{label}" } small { "Pendiente" } }
                                    }
                                }
                            }

                            div { class: "wizard-body",
                                if wizard_step == 0 {
                                    div { class: "wizard-step-copy",
                                        span { class: "wizard-step-icon", "01" }
                                        div {
                                            span { class: "eyebrow", "INFORMACIÓN GENERAL" }
                                            h4 { "Datos del solicitante" }
                                            p { "Confirma la información que aparecerá en la rectificación. Se guardará localmente y quedará disponible para futuros expedientes." }
                                        }
                                    }
                                    div { class: "wizard-profile-fields",
                                        label { class: "wide-field", span { "Nombre completo" } input {
                                            value: "{state_snapshot.applicant_profile.full_name}",
                                            oninput: move |event| app_state.write().applicant_profile.full_name = event.value(),
                                        } }
                                        label { span { "Correo" } input {
                                            value: "{state_snapshot.applicant_profile.email}",
                                            oninput: move |event| app_state.write().applicant_profile.email = event.value(),
                                        } }
                                        label { span { "Teléfono" } input {
                                            value: "{state_snapshot.applicant_profile.phone}",
                                            oninput: move |event| app_state.write().applicant_profile.phone = event.value(),
                                        } }
                                        label { class: "wide-field", span { "Dirección" } input {
                                            value: "{state_snapshot.applicant_profile.address}",
                                            oninput: move |event| app_state.write().applicant_profile.address = event.value(),
                                        } }
                                        label { span { "Ciudad" } input {
                                            value: "{state_snapshot.applicant_profile.city}",
                                            oninput: move |event| app_state.write().applicant_profile.city = event.value(),
                                        } }
                                        label { span { "Estado" } input {
                                            value: "{state_snapshot.applicant_profile.state}",
                                            oninput: move |event| app_state.write().applicant_profile.state = event.value(),
                                        } }
                                        label { span { "Código postal" } input {
                                            value: "{state_snapshot.applicant_profile.postal_code}",
                                            maxlength: 5,
                                            inputmode: "numeric",
                                            oninput: move |event| app_state.write().applicant_profile.postal_code = event.value(),
                                        } }
                                    }
                                } else {
                                    div { class: "wizard-step-copy",
                                        span { class: "wizard-step-icon", "02" }
                                        div {
                                            span { class: "eyebrow", "ENVÍO Y GUÍA" }
                                            h4 { "Identifica correctamente tu paquete" }
                                            p { "Registra la guía, el folio aduanal y un nombre local. El seguimiento completo aparecerá después en el expediente." }
                                        }
                                    }
                                    div { class: "wizard-profile-fields",
                                        label { class: "wide-field", span { "Número de guía" } input {
                                            value: "{tracking_number}", maxlength: 13, autocomplete: "off", autofocus: true,
                                            placeholder: "AA123456789BB",
                                            oninput: move |event| { tracking_number.set(event.value().to_ascii_uppercase()); form_error.set(None); },
                                        } small { "Debe contener exactamente 13 caracteres alfanuméricos." } }
                                        label { span { "Empresa postal" } input { value: "Correos de México", readonly: true } }
                                        label { span { "Folio de boleta" } input {
                                            value: "{customs_form_number}", placeholder: "Opcional por ahora",
                                            oninput: move |event| customs_form_number.set(event.value()),
                                        } }
                                        label { class: "wide-field", span { "Nombre del expediente" } input {
                                            value: "{display_name}", placeholder: "Ej. Audífonos Japón",
                                            oninput: move |event| display_name.set(event.value()),
                                        } }
                                        div { class: "wizard-tracking-note wide-field", strong { "✓ Guía registrada" } span { "El seguimiento estará disponible en el expediente." } }
                                    }
                                }
                            }

                            if let Some(error) = form_error.read().as_ref() {
                                div { class: "form-error", "{error}" }
                            }

                            div { class: "form-actions",
                                div { class: "security-note",
                                    span { "●" }
                                    "Se guardará únicamente en este equipo"
                                }
                                div { class: "wizard-actions",
                                    if wizard_step > 0 {
                                        button {
                                            class: "text-button",
                                            r#type: "button",
                                            onclick: move |_| {
                                                new_case_step.set(wizard_step.saturating_sub(1));
                                                form_error.set(None);
                                            },
                                            "Atrás"
                                        }
                                    }
                                    button { class: "primary-button", r#type: "submit", "Guardar y continuar" }
                                }
                            }
                        }
                    }
                }
            }
        }

        if state_snapshot.show_onboarding {
            Onboarding {
                profile: state_snapshot.applicant_profile.clone(),
                on_completed: move |profile| {
                    let mut state = app_state.write();
                    state.applicant_profile = profile;
                    state.show_onboarding = false;
                },
            }
        }
    }
}

#[component]
fn FaqPage() -> Element {
    rsx! {
        section { class: "faq-page",
            div { class: "settings-heading",
                span { class: "eyebrow", "GUÍA DE APOYO" }
                h3 { "Dudas comunes sobre la rectificación" }
                p { "Orientación práctica para preparar y presentar el expediente. Confirma siempre la vigencia del procedimiento y los requisitos de la autoridad que atienda tu caso." }
            }

            div { class: "faq-notice",
                strong { "Importante" }
                span { "Esta sección ayuda a organizar el trámite; no sustituye la respuesta oficial de Correos de México ni de la autoridad aduanera." }
            }

            div { class: "faq-list",
                details { class: "faq-item", open: true,
                    summary { "No quisieron recibir la rectificación en mi oficina postal. ¿Qué hago?" }
                    div { class: "faq-answer",
                        p { "Te sugerimos indicar al personal de la ventanilla que el procedimiento puede verificarse en su «Manual de Procedimientos para la Prestación de Servicios en Ventanilla»." }
                        div { class: "procedure-reference",
                            strong { "Apartado 8.12" }
                            span { "«Oficio de Remisión o Reexpedición de Piezas del Exterior, Rehusadas, No Reclamadas o para Rectificación de Derechos de Importación», forma SPM-72." }
                        }
                        p { "En ese apartado se indican los datos que debe contener el escrito dirigido al titular de la Aduana del Aeropuerto Internacional Benito Juárez de la Ciudad de México." }
                        p { "Además del escrito, adjunta copia de tu INE por ambos lados y la factura o el ticket de compra que compruebe cuánto costó el contenido del envío. También puedes presentar el estado de cuenta donde se confirme el pago." }
                        p { "En caso de que nuevamente te otorguen una negativa, te pedimos, por favor, que nos lo indiques con el fin de turnarlo al área correspondiente. Conserva el motivo de la negativa y, de ser posible, el nombre de la oficina y la fecha de atención." }
                    }
                }

                details { class: "faq-item",
                    summary { "¿Qué documentos conviene llevar para solicitar la rectificación?" }
                    div { class: "faq-answer",
                        p { "Lleva el escrito firmado, la boleta o determinación aduanal, copia de tu INE por ambos lados, comprobante de compra y evidencia del pago. También conviene incluir el rastreo y cualquier documento que permita identificar claramente la mercancía, su cantidad y su valor real." }
                        p { "Conserva los originales y entrega copias cuando corresponda. La app genera la solicitud y el dossier, pero debes revisar que cada anexo sea legible y pertenezca al expediente correcto." }
                    }
                }

                details { class: "faq-item",
                    summary { "¿Qué ocurre si el valor real no supera los 50 USD?" }
                    div { class: "faq-answer",
                        p { "La app incluye una evaluación preliminar del umbral postal de hasta 50 USD y redacta la solicitud de retiro de contribuciones cuando se cumplen los demás requisitos aplicables. No se debe interpretar como una exención automática: confirma el tipo de cambio, la regla vigente y las características del envío antes de presentar el escrito." }
                    }
                }

                details { class: "faq-item",
                    summary { "¿Y si el valor real supera los 50 USD?" }
                    div { class: "faq-answer",
                        p { "Para valores superiores a 50 USD y de hasta 1,000 USD, la app muestra de forma preliminar el cálculo del 19 % sobre el valor real acreditado. El escrito aclara que no te niegas a cubrir las contribuciones procedentes, sino que solicitas que se calculen sobre una valoración adecuada." }
                        p { "Si el valor supera los 1,000 USD, la app no aplica ese cálculo simplificado y solicita confirmar el procedimiento correspondiente." }
                    }
                }

                details { class: "faq-item",
                    summary { "¿El envío o los descuentos se consideran en la valoración?" }
                    div { class: "faq-answer",
                        p { "En los cálculos de esta app, el costo de envío se conserva únicamente como dato informativo y no se suma al valor de la mercancía. Tampoco se aplican descuentos ni se solicita información del vendedor. Revisa el criterio oficial aplicable antes de presentar el expediente." }
                    }
                }

                details { class: "faq-item",
                    summary { "¿Qué hago si el rastreo automático no muestra movimientos?" }
                    div { class: "faq-answer",
                        p { "Abre el portal oficial desde el panel de rastreo y verifica la guía directamente. Si el portal cambió o no responde, puedes registrar el movimiento manualmente; la ausencia de actualizaciones no se interpreta automáticamente como entrega, rechazo o pérdida." }
                    }
                }

                details { class: "faq-item",
                    summary { "¿La aplicación envía el correo automáticamente?" }
                    div { class: "faq-answer",
                        p { "No. La app prepara un archivo .eml dirigido de forma predeterminada a cdabjtramites@correosdemexico.gob.mx, con la solicitud y el dossier adjuntos. Solo se abre en tu cliente de correo cuando pulsas el botón correspondiente, y el envío depende de tu revisión y confirmación dentro de ese cliente." }
                    }
                }

                details { class: "faq-item",
                    summary { "¿Dónde se guardan mis datos y documentos?" }
                    div { class: "faq-answer",
                        p { "El expediente se guarda localmente en este equipo. Las evidencias originales se almacenan cifradas; el PDF listo para imprimir, el Word editable, los PDF de la solicitud y las pruebas, el correo y el ZIP exportados quedan sin cifrar en la carpeta que tú elijas para poder revisarlos y enviarlos." }
                    }
                }

                details { class: "faq-item",
                    summary { "¿Qué debo revisar antes de presentar o enviar el expediente?" }
                    div { class: "faq-answer",
                        p { "Confirma tu nombre y domicilio, guía, folio de boleta, productos, cantidades, importes, monedas, tasas de conversión, destinatario oficial y anexos. Comprueba también que el PDF y el Word abran correctamente, que cada evidencia sea legible y que no incluyan información personal innecesaria." }
                    }
                }
            }
        }
    }
}

fn validate_applicant_profile(profile: &ApplicantProfile) -> Result<(), &'static str> {
    if profile.full_name.is_empty() {
        return Err("Escribe el nombre completo del solicitante");
    }
    if !profile.email.is_empty()
        && (!profile.email.contains('@')
            || profile.email.starts_with('@')
            || profile.email.ends_with('@'))
    {
        return Err("El correo electrónico no tiene un formato válido");
    }
    if !profile.postal_code.is_empty()
        && (profile.postal_code.len() != 5
            || !profile
                .postal_code
                .chars()
                .all(|character| character.is_ascii_digit()))
    {
        return Err("El código postal debe contener exactamente cinco dígitos");
    }
    Ok(())
}

fn validate_wizard_tracking_number(tracking_number: &str) -> Result<(), String> {
    RectificationCase::new(tracking_number, None, None)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[component]
fn OnboardingIcon(name: &'static str) -> Element {
    let paths: &[&str] = match name {
        "clipboard" => &[
            "M9 5H7a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V7a2 2 0 0 0-2-2h-2",
            "M9 3h6v4H9z",
            "m9 14 2 2 4-4",
        ],
        "calculator" => &[
            "M4 2h16v20H4z",
            "M8 6h8v4H8z",
            "M8 14h.01M12 14h.01M16 14h.01M8 18h.01M12 18h.01M16 18h.01",
        ],
        "files" => &[
            "M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z",
            "M14 2v6h6",
            "M8 13h8M8 17h8",
        ],
        "mail" => &["M4 4h16v16H4z", "m4 7 8 6 8-6"],
        "tracking" => &[
            "M20 10c0 5-8 11-8 11S4 15 4 10a8 8 0 1 1 16 0z",
            "M12 7v3l2 2",
        ],
        "package" => &[
            "m21 8-9-5-9 5 9 5z",
            "m3 8 9 5 9-5",
            "M12 13v9M21 8v9l-9 5-9-5V8",
        ],
        "paperclip" => &[
            "m21.4 11.6-8.9 8.9a6 6 0 0 1-8.5-8.5l9.6-9.6a4 4 0 0 1 5.7 5.7l-9.6 9.6a2 2 0 0 1-2.8-2.8l8.9-8.9",
        ],
        "file-check" => &[
            "M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z",
            "M14 2v6h6",
            "m9 15 2 2 4-4",
        ],
        "info" => &[
            "M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20z",
            "M12 10v6M12 7h.01",
        ],
        "shield" => &[
            "M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z",
            "M9 12h6v5H9z",
            "M10 12V9a2 2 0 0 1 4 0v3",
        ],
        "gift" => &[
            "M3 8h18v4H3zM5 12v9h14v-9M12 8v13",
            "M12 8H7.5a2.5 2.5 0 1 1 2.2-3.7L12 8zm0 0h4.5a2.5 2.5 0 1 0-2.2-3.7L12 8z",
        ],
        "home" => &["m3 11 9-8 9 8", "M5 10v11h14V10M9 21v-6h6v6"],
        "heart" => &[
            "M20.8 4.6a5.5 5.5 0 0 0-7.8 0L12 5.7l-1.1-1.1a5.5 5.5 0 0 0-7.8 7.8L12 21l8.8-8.6a5.5 5.5 0 0 0 0-7.8z",
        ],
        "lock" => &["M6 10h12v11H6z", "M8 10V7a4 4 0 0 1 8 0v3", "M12 14v3"],
        _ => &["M4 4h16v16H4z"],
    };

    rsx! {
        svg {
            class: "onboarding-svg-icon",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.8",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            for path_data in paths {
                path { d: "{path_data}" }
            }
        }
    }
}

#[component]
fn Onboarding(profile: ApplicantProfile, on_completed: EventHandler<ApplicantProfile>) -> Element {
    let ApplicantProfile {
        full_name: saved_name,
        email: saved_email,
        phone: saved_phone,
        address: saved_address,
        city: saved_city,
        state: saved_state,
        postal_code: saved_postal_code,
    } = profile;
    let mut step = use_signal(|| 0usize);
    let mut full_name = use_signal(move || saved_name);
    let mut email = use_signal(move || saved_email);
    let mut phone = use_signal(move || saved_phone);
    let mut address = use_signal(move || saved_address);
    let mut city = use_signal(move || saved_city);
    let mut state = use_signal(move || saved_state);
    let mut postal_code = use_signal(move || saved_postal_code);
    let mut error = use_signal(|| None::<String>);
    let current_step = *step.read();
    let step_labels = [
        "Bienvenida",
        "Cómo funciona",
        "Privacidad",
        "Tu información",
    ];

    rsx! {
        section { class: "onboarding-overlay",
            div { class: "onboarding-shell",
                aside { class: "onboarding-rail",
                    div { class: "onboarding-brand",
                        img {
                            src: logo_data_uri(),
                            alt: "Mi Rectificación MX",
                        }
                    }
                    div { class: "onboarding-rail-copy",
                        span { "PRIMER INICIO" }
                        strong { "Conoce tu espacio de rectificación" }
                    }
                    nav { class: "onboarding-steps",
                        for (index, label) in step_labels.iter().enumerate() {
                            button {
                                key: "{index}",
                                r#type: "button",
                                class: if index == current_step { "active" } else if index < current_step { "done" } else { "" },
                                disabled: index > current_step,
                                onclick: move |_| {
                                    error.set(None);
                                    step.set(index);
                                },
                                span { if index < current_step { "✓" } else { "{index + 1}" } }
                                div {
                                    small { "PASO {index + 1}" }
                                    strong { "{label}" }
                                }
                            }
                        }
                    }
                    div { class: "onboarding-local-note",
                        span { OnboardingIcon { name: "home" } }
                        div {
                            strong { "Perfil y expedientes locales" }
                            small { "Tus datos personales se guardan únicamente en este equipo." }
                        }
                    }
                }

                main { class: "onboarding-content",
                    div { class: "onboarding-topline",
                        span { "PASO {current_step + 1} DE 4" }
                        div { class: "onboarding-dots",
                            for index in 0..4 {
                                span { key: "dot-{index}", class: if index <= current_step { "active" } else { "" } }
                            }
                        }
                    }

                    if current_step == 0 {
                        div { class: "onboarding-page onboarding-welcome",
                            div { class: "onboarding-hero-icon", OnboardingIcon { name: "clipboard" } }
                            span { class: "eyebrow", "BIENVENIDO A MI RECTIFICACIÓN MX" }
                            h1 { "Prepara una rectificación clara y bien sustentada" }
                            p { class: "onboarding-lead", "La aplicación te ayuda a ordenar la información de un envío postal con una valoración aduanera incorrecta, reunir tus pruebas y preparar los documentos para que tú los revises antes de presentarlos." }
                            div { class: "onboarding-benefits",
                                article {
                                    span { OnboardingIcon { name: "calculator" } }
                                    strong { "Valuación ordenada" }
                                    p { "Registra productos y convierte su valor original a pesos mexicanos sin sumar el envío." }
                                }
                                article {
                                    span { OnboardingIcon { name: "files" } }
                                    strong { "Pruebas reunidas" }
                                    p { "Agrupa comprobantes, estados de cuenta, boletas y capturas en un expediente." }
                                }
                                article {
                                    span { OnboardingIcon { name: "mail" } }
                                    strong { "Documentos preparados" }
                                    p { "Genera un PDF listo para imprimir, una solicitud editable en Word y un borrador de correo para revisión." }
                                }
                            }
                        }
                    } else if current_step == 1 {
                        div { class: "onboarding-page",
                            span { class: "eyebrow", "UN FLUJO GUIADO" }
                            h1 { "De la guía postal al expediente listo" }
                            p { class: "onboarding-lead", "Cada módulo conserva su información para que puedas avanzar poco a poco y volver a revisar cualquier dato." }
                            div { class: "onboarding-flow",
                                article {
                                    div { class: "flow-icon", OnboardingIcon { name: "tracking" } }
                                    span { "01" }
                                    strong { "Registra el envío" }
                                    p { "Añade la guía y el folio de la boleta para identificar el caso y consultar su rastreo." }
                                }
                                article {
                                    div { class: "flow-icon", OnboardingIcon { name: "package" } }
                                    span { "02" }
                                    strong { "Declara los productos" }
                                    p { "Captura nombre, cantidad, precio y moneda original; la conversión queda documentada." }
                                }
                                article {
                                    div { class: "flow-icon", OnboardingIcon { name: "paperclip" } }
                                    span { "03" }
                                    strong { "Adjunta las pruebas" }
                                    p { "Importa transacciones, estado de cuenta, boleta y demás evidencia necesaria." }
                                }
                                article {
                                    div { class: "flow-icon", OnboardingIcon { name: "file-check" } }
                                    span { "04" }
                                    strong { "Revisa y prepara" }
                                    p { "Comprueba el PDF, el Word y el correo. La decisión de presentarlos o enviarlos siempre es tuya." }
                                }
                            }
                            div { class: "onboarding-review-note",
                                span { OnboardingIcon { name: "info" } }
                                p { strong { "La app no envía nada por sí sola. " } "Siempre tendrás la oportunidad de revisar importes, documentos, destinatario y anexos." }
                            }
                        }
                    } else if current_step == 2 {
                        div { class: "onboarding-page onboarding-privacy",
                            div { class: "privacy-shield", OnboardingIcon { name: "shield" } }
                            span { class: "eyebrow", "PRIVACIDAD Y ACCESO" }
                            h1 { "Gratuita, local y bajo tu control" }
                            p { class: "onboarding-lead", "Mi Rectificación MX no requiere una cuenta ni sincroniza tu perfil o tus expedientes con una nube de la aplicación." }
                            div { class: "privacy-grid",
                                article {
                                    span { OnboardingIcon { name: "gift" } }
                                    div {
                                        strong { "Aplicación totalmente gratuita" }
                                        p { "No hay funciones de pago. Queda prohibida su venta, reventa o comercialización." }
                                    }
                                }
                                article {
                                    span { OnboardingIcon { name: "home" } }
                                    div {
                                        strong { "Almacenamiento en este equipo" }
                                        p { "Tu perfil y expedientes permanecen localmente; las evidencias se almacenan cifradas." }
                                    }
                                }
                                article {
                                    span { OnboardingIcon { name: "heart" } }
                                    div {
                                        strong { "Apoyo siempre voluntario" }
                                        p { "Puedes apoyar al desarrollador, pero una aportación nunca condiciona funciones, actualizaciones ni soporte." }
                                    }
                                }
                            }
                            div { class: "export-privacy-note",
                                strong { "Tú eliges qué sale del equipo" }
                                p { "Solo al exportar o abrir el borrador de correo se crean archivos para que los revises y decidas cómo compartirlos. Las consultas de rastreo y tipo de cambio sí contactan las fuentes indicadas en cada módulo." }
                            }
                        }
                    } else {
                        form {
                            class: "onboarding-page onboarding-profile",
                            onsubmit: move |event| {
                                event.prevent_default();
                                error.set(None);
                                let profile = ApplicantProfile {
                                    full_name: full_name.read().trim().to_owned(),
                                    email: email.read().trim().to_owned(),
                                    phone: phone.read().trim().to_owned(),
                                    address: address.read().trim().to_owned(),
                                    city: city.read().trim().to_owned(),
                                    state: state.read().trim().to_owned(),
                                    postal_code: postal_code.read().trim().to_owned(),
                                };
                                if let Err(message) = validate_applicant_profile(&profile) {
                                    error.set(Some(message.to_owned()));
                                    return;
                                }
                                let result = CaseDataStore::open_default()
                                    .and_then(|store| store.complete_onboarding(&profile))
                                    .map_err(|value| value.to_string());
                                match result {
                                    Ok(()) => on_completed.call(profile),
                                    Err(message) => error.set(Some(message)),
                                }
                            },
                            span { class: "eyebrow", "AUTORRELLENO LOCAL" }
                            h1 { "Cuéntanos quién presenta la solicitud" }
                            p { class: "onboarding-lead", "Estos datos se usarán para autorrellenar tus rectificaciones. Podrás modificarlos después desde Configuración o dentro de cada expediente." }
                            div { class: "onboarding-profile-fields",
                                label { class: "wide-field",
                                    span { "Nombre completo" }
                                    input { value: "{full_name}", autocomplete: "name", placeholder: "Nombre de la persona solicitante", oninput: move |event| full_name.set(event.value()) }
                                }
                                label {
                                    span { "Correo electrónico" }
                                    input { r#type: "email", value: "{email}", autocomplete: "email", placeholder: "nombre@correo.com", oninput: move |event| email.set(event.value()) }
                                }
                                label {
                                    span { "Teléfono" }
                                    input { value: "{phone}", autocomplete: "tel", placeholder: "Opcional", oninput: move |event| phone.set(event.value()) }
                                }
                                label { class: "wide-field",
                                    span { "Dirección" }
                                    input { value: "{address}", autocomplete: "street-address", placeholder: "Calle, número y colonia", oninput: move |event| address.set(event.value()) }
                                }
                                label {
                                    span { "Ciudad" }
                                    input { value: "{city}", autocomplete: "address-level2", placeholder: "Ej. Hermosillo", oninput: move |event| city.set(event.value()) }
                                }
                                label {
                                    span { "Estado" }
                                    input { value: "{state}", autocomplete: "address-level1", placeholder: "Ej. Sonora", oninput: move |event| state.set(event.value()) }
                                }
                                label {
                                    span { "Código postal" }
                                    input { value: "{postal_code}", autocomplete: "postal-code", inputmode: "numeric", maxlength: "5", placeholder: "00000", oninput: move |event| postal_code.set(event.value()) }
                                }
                            }
                            div { class: "onboarding-profile-note",
                                span { OnboardingIcon { name: "lock" } }
                                "Esta información se guardará únicamente en este equipo."
                            }
                            if let Some(message) = error.read().as_ref() {
                                div { class: "form-error onboarding-error", "{message}" }
                            }
                            div { class: "onboarding-actions",
                                button { class: "secondary-button", r#type: "button", onclick: move |_| { error.set(None); step.set(2); }, "Atrás" }
                                button { class: "primary-button", r#type: "submit", "Guardar y comenzar" }
                            }
                        }
                    }

                    if current_step < 3 {
                        div { class: "onboarding-actions",
                            button {
                                class: "secondary-button",
                                r#type: "button",
                                disabled: current_step == 0,
                                onclick: move |_| step.set(current_step.saturating_sub(1)),
                                "Atrás"
                            }
                            button {
                                class: "primary-button",
                                r#type: "button",
                                onclick: move |_| step.set((current_step + 1).min(3)),
                                if current_step == 2 { "Configurar mi perfil" } else { "Continuar" }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn SocialIcon(platform: &'static str) -> Element {
    let paths: &[&str] = match platform {
        "facebook" => &["M14 8h3V4h-3c-3 0-5 2-5 5v3H6v4h3v6h4v-6h3l1-4h-4V9c0-.7.3-1 1-1z"],
        "github" => &[
            "M12 2a10 10 0 0 0-3.16 19.49c.5.09.68-.22.68-.48v-1.87c-2.78.6-3.37-1.18-3.37-1.18-.45-1.15-1.1-1.46-1.1-1.46-.9-.62.07-.6.07-.6 1 .07 1.52 1.02 1.52 1.02.89 1.52 2.34 1.08 2.91.83.09-.65.35-1.08.63-1.33-2.22-.25-4.56-1.11-4.56-4.94 0-1.09.39-1.98 1.03-2.68-.1-.25-.45-1.27.1-2.64 0 0 .84-.27 2.75 1.02A9.6 9.6 0 0 1 12 6.84a9.6 9.6 0 0 1 2.5.34c1.91-1.29 2.75-1.02 2.75-1.02.55 1.37.2 2.39.1 2.64.64.7 1.03 1.59 1.03 2.68 0 3.84-2.34 4.68-4.57 4.93.36.31.68.92.68 1.85v2.75c0 .27.18.58.69.48A10 10 0 0 0 12 2z",
        ],
        "kofi" => &[
            "M4 5h13v3h1.5a3.5 3.5 0 0 1 0 7H17v1a4 4 0 0 1-4 4H8a4 4 0 0 1-4-4z",
            "M17 10v3h1.5a1.5 1.5 0 0 0 0-3z",
            "M7.5 9.5c1-1 2.5-.3 3 1 .5-1.3 2-2 3-1 1.5 1.5-1 4-3 5.5-2-1.5-4.5-4-3-5.5z",
        ],
        _ => &[],
    };
    rsx! {
        svg {
            view_box: "0 0 24 24",
            fill: if platform == "github" { "currentColor" } else { "none" },
            stroke: if platform == "github" { "none" } else { "currentColor" },
            stroke_width: "1.8",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            for path in paths { path { d: "{path}" } }
        }
    }
}

#[component]
fn SettingsPage(profile: ApplicantProfile, on_saved: EventHandler<ApplicantProfile>) -> Element {
    let ApplicantProfile {
        full_name: saved_name,
        email: saved_email,
        phone: saved_phone,
        address: saved_address,
        city: saved_city,
        state: saved_state,
        postal_code: saved_postal_code,
    } = profile;
    let mut full_name = use_signal(move || saved_name);
    let mut email = use_signal(move || saved_email);
    let mut phone = use_signal(move || saved_phone);
    let mut address = use_signal(move || saved_address);
    let mut city = use_signal(move || saved_city);
    let mut state = use_signal(move || saved_state);
    let mut postal_code = use_signal(move || saved_postal_code);
    let mut notice = use_signal(|| None::<String>);
    let mut error = use_signal(|| None::<String>);

    rsx! {
        section { class: "settings-page",
            div { class: "settings-heading",
                span { class: "eyebrow", "PERFIL DEL SOLICITANTE" }
                h3 { "Tus datos para autorrelleno" }
                p { "Se guardan únicamente en este equipo y se colocan automáticamente en los formularios de cada rectificación." }
            }

            if let Some(message) = notice.read().as_ref() {
                div { class: "alert success", "{message}" }
            }
            if let Some(message) = error.read().as_ref() {
                div { class: "alert error", "{message}" }
            }

            form {
                class: "settings-form",
                onsubmit: move |event| {
                    event.prevent_default();
                    notice.set(None);
                    error.set(None);
                    let profile = ApplicantProfile {
                        full_name: full_name.read().trim().to_owned(),
                        email: email.read().trim().to_owned(),
                        phone: phone.read().trim().to_owned(),
                        address: address.read().trim().to_owned(),
                        city: city.read().trim().to_owned(),
                        state: state.read().trim().to_owned(),
                        postal_code: postal_code.read().trim().to_owned(),
                    };
                    if let Err(message) = validate_applicant_profile(&profile) {
                        error.set(Some(message.to_owned()));
                        return;
                    }
                    let result = CaseDataStore::open_default()
                        .map_err(|value| value.to_string())
                        .and_then(|store| store.save_applicant_profile(&profile).map_err(|value| value.to_string()));
                    match result {
                        Ok(()) => {
                            on_saved.call(profile);
                            notice.set(Some("Perfil guardado. Los formularios se autorrellenarán con estos datos.".to_owned()));
                        }
                        Err(message) => error.set(Some(message)),
                    }
                },
                div { class: "settings-card",
                    div { class: "settings-card-heading",
                        div {
                            strong { "Información personal" }
                            span { "Podrás modificar estos valores dentro de cada expediente antes de generar." }
                        }
                        span { class: "local-badge", "SOLO LOCAL" }
                    }
                    div { class: "settings-fields",
                        label { class: "wide-field",
                            span { "Nombre completo" }
                            input { value: "{full_name}", autocomplete: "name", placeholder: "Nombre de la persona solicitante", oninput: move |event| full_name.set(event.value()) }
                        }
                        label {
                            span { "Correo electrónico" }
                            input { r#type: "email", value: "{email}", autocomplete: "email", placeholder: "nombre@correo.com", oninput: move |event| email.set(event.value()) }
                        }
                        label {
                            span { "Teléfono" }
                            input { value: "{phone}", autocomplete: "tel", placeholder: "Opcional", oninput: move |event| phone.set(event.value()) }
                        }
                        label { class: "wide-field",
                            span { "Dirección" }
                            input { value: "{address}", autocomplete: "street-address", placeholder: "Calle, número y colonia", oninput: move |event| address.set(event.value()) }
                        }
                        label {
                            span { "Ciudad" }
                            input { value: "{city}", autocomplete: "address-level2", placeholder: "Ej. Ciudad Obregón", oninput: move |event| city.set(event.value()) }
                        }
                        label {
                            span { "Estado" }
                            input { value: "{state}", autocomplete: "address-level1", placeholder: "Ej. Sonora", oninput: move |event| state.set(event.value()) }
                        }
                        label {
                            span { "Código postal" }
                            input { value: "{postal_code}", autocomplete: "postal-code", inputmode: "numeric", maxlength: "5", placeholder: "00000", oninput: move |event| postal_code.set(event.value()) }
                        }
                    }
                    div { class: "settings-actions",
                        div { class: "security-note",
                            span { "●" }
                            "Esta información no se sincroniza ni se envía automáticamente"
                        }
                        button { class: "primary-button", r#type: "submit", "Guardar perfil" }
                    }
                }
            }

            div { class: "free-app-notice",
                div { class: "free-app-icon", "♥" }
                div {
                    strong { "Aplicación totalmente gratuita" }
                    p { "Queda prohibida la venta, reventa o comercialización de esta aplicación." }
                    span { "Si esta herramienta te resulta útil, puedes apoyar voluntariamente al desarrollador. Las aportaciones no condicionan sus funciones, actualizaciones ni soporte." }
                }
            }
            div { class: "developer-socials",
                div {
                    strong { "Proyecto y comunidad" }
                    span { "Síguenos o apoya el desarrollo de Mi Rectificación MX." }
                }
                nav { aria_label: "Redes y apoyo del proyecto",
                    button {
                        r#type: "button",
                        title: "Facebook",
                        aria_label: "Abrir Facebook",
                        onclick: move |_| if let Err(value) = open::that(FACEBOOK_URL) { error.set(Some(value.to_string())); },
                        SocialIcon { platform: "facebook" }
                    }
                    button {
                        r#type: "button",
                        title: "GitHub",
                        aria_label: "Abrir GitHub",
                        onclick: move |_| if let Err(value) = open::that(GITHUB_URL) { error.set(Some(value.to_string())); },
                        SocialIcon { platform: "github" }
                    }
                    button {
                        r#type: "button",
                        title: "Ko-fi",
                        aria_label: "Abrir Ko-fi",
                        onclick: move |_| if let Err(value) = open::that(KOFI_URL) { error.set(Some(value.to_string())); },
                        SocialIcon { platform: "kofi" }
                    }
                }
            }
        }
    }
}

#[derive(Clone)]
struct EvidencePanelState {
    vault: Option<EvidenceVault>,
    documents: Vec<EvidenceDocument>,
    audit_events: Vec<AuditEvent>,
    error: Option<String>,
}

impl EvidencePanelState {
    fn load(case_id: Uuid) -> Self {
        match EvidenceVault::open_default() {
            Ok(vault) => {
                let mut state = Self {
                    vault: Some(vault),
                    documents: Vec::new(),
                    audit_events: Vec::new(),
                    error: None,
                };
                state.reload(case_id);
                state
            }
            Err(error) => Self {
                vault: None,
                documents: Vec::new(),
                audit_events: Vec::new(),
                error: Some(error.to_string()),
            },
        }
    }

    fn reload(&mut self, case_id: Uuid) {
        let Some(vault) = self.vault.as_ref() else {
            return;
        };
        match (
            vault.list_evidence(case_id),
            vault.list_audit_events(case_id),
        ) {
            (Ok(documents), Ok(audit_events)) => {
                self.documents = documents;
                self.audit_events = audit_events;
                self.error = None;
            }
            (Err(error), _) | (_, Err(error)) => self.error = Some(error.to_string()),
        }
    }
}

#[derive(Clone)]
struct EvidencePreview {
    document: EvidenceDocument,
    data_url: String,
}

#[derive(Clone)]
struct ProductPanelState {
    store: Option<CaseDataStore>,
    products: Vec<ProductLine>,
    error: Option<String>,
}

impl ProductPanelState {
    fn load(case_id: Uuid) -> Self {
        match CaseDataStore::open_default() {
            Ok(store) => {
                let mut state = Self {
                    store: Some(store),
                    products: Vec::new(),
                    error: None,
                };
                state.reload(case_id);
                state
            }
            Err(error) => Self {
                store: None,
                products: Vec::new(),
                error: Some(error.to_string()),
            },
        }
    }

    fn reload(&mut self, case_id: Uuid) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        match store.list_products(case_id) {
            Ok(products) => {
                self.products = products;
                self.error = None;
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }
}

#[derive(Clone)]
struct TrackingPanelState {
    store: Option<CaseDataStore>,
    events: Vec<TrackingEvent>,
    refresh_state: TrackingRefreshState,
    error: Option<String>,
}

#[derive(Clone)]
struct EmailPanelState {
    store: Option<CaseDataStore>,
    draft: Option<EmailDraft>,
    error: Option<String>,
}

impl EmailPanelState {
    fn load(case_id: Uuid) -> Self {
        match CaseDataStore::open_default() {
            Ok(store) => match store.load_email_draft(case_id) {
                Ok(draft) => Self {
                    store: Some(store),
                    draft,
                    error: None,
                },
                Err(error) => Self {
                    store: Some(store),
                    draft: None,
                    error: Some(error.to_string()),
                },
            },
            Err(error) => Self {
                store: None,
                draft: None,
                error: Some(error.to_string()),
            },
        }
    }

    fn reload(&mut self, case_id: Uuid) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        match store.load_email_draft(case_id) {
            Ok(draft) => {
                self.draft = draft;
                self.error = None;
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }
}

impl TrackingPanelState {
    fn load(case_id: Uuid) -> Self {
        match CaseDataStore::open_default() {
            Ok(store) => {
                let mut state = Self {
                    store: Some(store),
                    events: Vec::new(),
                    refresh_state: TrackingRefreshState::default(),
                    error: None,
                };
                state.reload(case_id);
                state
            }
            Err(error) => Self {
                store: None,
                events: Vec::new(),
                refresh_state: TrackingRefreshState::default(),
                error: Some(error.to_string()),
            },
        }
    }

    fn reload(&mut self, case_id: Uuid) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        match (
            store.list_tracking_events(case_id),
            store.tracking_refresh_state(case_id),
        ) {
            (Ok(events), Ok(refresh_state)) => {
                self.events = events;
                self.refresh_state = refresh_state;
                self.error = None;
            }
            (Err(error), _) | (_, Err(error)) => self.error = Some(error.to_string()),
        }
    }
}

fn refresh_tracking_case(
    case_id: Uuid,
    tracking_number: &str,
) -> Result<TrackingUpdateResult, String> {
    let store = CaseDataStore::open_default().map_err(|error| error.to_string())?;
    let provider = CorreosMexicoProvider::new().map_err(|error| error.to_string())?;
    match provider.track(tracking_number, Local::now().year()) {
        Ok(response) => store
            .record_tracking_response(
                case_id,
                "correos_mexico",
                &response.raw_response,
                response.fetched_at,
                &response.events,
            )
            .map_err(|error| error.to_string()),
        Err(error) => {
            let message = error.to_string();
            let _ =
                store.record_tracking_error(case_id, "correos_mexico", "", &message, Utc::now());
            Err(message)
        }
    }
}

fn automatic_tracking_refresh_due_for_case(case_id: Uuid) -> Result<bool, String> {
    let store = CaseDataStore::open_default().map_err(|error| error.to_string())?;
    let state = store
        .tracking_refresh_state(case_id)
        .map_err(|error| error.to_string())?;
    Ok(automatic_tracking_refresh_due(
        state.last_attempt_at,
        Utc::now(),
    ))
}

fn automatic_tracking_refresh_due(
    last_attempt_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    last_attempt_at.is_none_or(|last_attempt| {
        now.signed_duration_since(last_attempt)
            >= chrono::Duration::hours(TRACKING_REFRESH_INTERVAL_HOURS)
    })
}

fn fetch_exchange_rate(currency: &str, date: NaiveDate) -> Result<ExchangeRateSnapshot, String> {
    FrankfurterProvider::new()
        .map_err(|error| error.to_string())?
        .rate_to_mxn(currency, date)
        .map_err(|error| error.to_string())
}

fn parse_amount(value: &str, label: &str) -> Result<Decimal, String> {
    let normalized = value.trim().replace(',', ".");
    Decimal::from_str(&normalized).map_err(|_| format!("{label} no es un importe válido"))
}

fn format_money(value: Decimal) -> String {
    format!("{value:.2}")
}

#[derive(Clone, PartialEq, Eq)]
struct ProductFormRow {
    id: Uuid,
    name: String,
    quantity: String,
    unit_price: String,
    shipping: String,
    taxes: String,
}

#[derive(Clone)]
struct DocumentApplicantInput {
    full_name: String,
    email: String,
    phone: String,
    address: String,
    authority_name: String,
    authority_email: String,
    presumptive_value_mxn: String,
    city: String,
    state: String,
    postal_code: String,
    issuance_date: String,
    non_commercial_statement: String,
    request_notes: String,
}

fn export_case_document(
    case: &RectificationCase,
    input: DocumentApplicantInput,
    format: &str,
    destination: &Path,
) -> Result<std::path::PathBuf, String> {
    let store = CaseDataStore::open_default().map_err(|value| value.to_string())?;
    let products = store
        .list_products(case.id)
        .map_err(|value| value.to_string())?;
    let rate_date = products
        .iter()
        .map(|product| product.rate.rate_date)
        .max()
        .ok_or_else(|| "Agrega al menos un producto valorado antes de exportar".to_owned())?;
    let usd_rate = products
        .iter()
        .find(|product| product.currency == "USD" && product.rate.rate_date == rate_date)
        .map(|product| product.rate.clone())
        .map(Ok)
        .unwrap_or_else(|| fetch_exchange_rate("USD", rate_date))?;
    let applicant = ApplicantDetails {
        full_name: input.full_name,
        email: input.email,
        phone: input.phone,
        address: input.address,
        authority_name: input.authority_name,
        authority_email: input.authority_email,
        presumptive_value_mxn: input.presumptive_value_mxn,
        city: input.city,
        state: input.state,
        postal_code: input.postal_code,
        issuance_date: input.issuance_date,
        non_commercial_statement: input.non_commercial_statement,
        request_notes: input.request_notes,
        usd_rate,
    };
    let vault = EvidenceVault::open_default().map_err(|value| value.to_string())?;
    let evidence = vault
        .list_evidence(case.id)
        .map_err(|value| value.to_string())?
        .into_iter()
        .map(|document| {
            vault
                .load_evidence_bytes(&document)
                .map(|bytes| EvidenceAsset { document, bytes })
                .map_err(|value| value.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let exported = if format == "pdf" {
        export_print_ready_pdf(case, &applicant, &products, &evidence, destination)
    } else {
        export_editable_docx(case, &applicant, &products, destination)
    }
    .map_err(|value| value.to_string())?;
    let automatic_root = store
        .automatic_email_documents_root(case.id)
        .map_err(|value| value.to_string())?;
    let bundle = generate_bundle(case, &applicant, &products, &evidence, &automatic_root)
        .map_err(|value| value.to_string())?;
    store
        .save_email_draft(&EmailDraft {
            case_id: case.id,
            recipient: bundle.email_content.recipient.clone(),
            sender: bundle.email_content.sender.clone(),
            subject: bundle.email_content.subject.clone(),
            body: bundle.email_content.body.clone(),
            request_pdf_path: bundle.request_pdf.to_string_lossy().into_owned(),
            evidence_pdf_path: bundle.evidence_pdf.to_string_lossy().into_owned(),
            eml_path: bundle.email_draft.to_string_lossy().into_owned(),
            prepared_at: Utc::now(),
            opened_at: None,
            sent_at: None,
        })
        .map_err(|value| value.to_string())?;
    store
        .record_document_generation(case.id)
        .map_err(|value| value.to_string())?;
    Ok(exported)
}

impl ProductFormRow {
    fn empty() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: String::new(),
            quantity: "1".to_owned(),
            unit_price: String::new(),
            shipping: "0".to_owned(),
            taxes: "0".to_owned(),
        }
    }
}

#[component]
fn ProductPanel(case_id: Uuid, on_changed: EventHandler<()>) -> Element {
    let mut panel = use_signal(move || ProductPanelState::load(case_id));
    let mut show_product_form = use_signal(|| false);
    let mut product_rows = use_signal(|| vec![ProductFormRow::empty()]);
    let mut currency = use_signal(|| "JPY".to_owned());
    let mut rate_date = use_signal(|| Local::now().date_naive().to_string());
    let mut rate_mode = use_signal(|| "automatic".to_owned());
    let mut manual_rate = use_signal(String::new);
    let mut manual_source = use_signal(String::new);
    let mut manual_url = use_signal(String::new);
    let mut manual_reason = use_signal(String::new);
    let mut rate_snapshot = use_signal(|| None::<ExchangeRateSnapshot>);
    let mut rate_loading = use_signal(|| false);
    let mut pending_removal = use_signal(|| None::<Uuid>);
    let mut notice = use_signal(|| None::<String>);

    let snapshot = panel.read().clone();
    let product_rows_snapshot = product_rows.read().clone();
    let total_mxn = snapshot
        .products
        .iter()
        .fold(Decimal::ZERO, |total, product| total + product.total_mxn);
    let pending_product = pending_removal.read().and_then(|id| {
        snapshot
            .products
            .iter()
            .find(|product| product.id == id)
            .cloned()
    });

    rsx! {
        section { class: "product-section",
            div { class: "section-heading",
                div {
                    span { class: "eyebrow", "VALORACIÓN AUDITABLE" }
                    h4 { "Productos y conversión a MXN" }
                    p { "El valor se calcula sin descuentos ni costo de envío; el envío se conserva únicamente como referencia." }
                }
                div { class: "product-summary",
                    span { "{snapshot.products.len()} productos" }
                    strong { "${format_money(total_mxn)} MXN" }
                }
            }

            div { class: "reference-warning",
                strong { "Tasa de referencia" }
                span { "Frankfurter agrega datos de bancos centrales. Antes del envío se confirmará la fuente que exija Aduanas." }
            }

            if let Some(error) = snapshot.error.as_ref() {
                div { class: "alert error", "{error}" }
            }
            if let Some(message) = notice.read().as_ref() {
                div { class: "alert success", "{message}" }
            }

            if *show_product_form.read() {
              div { class: "product-form",
                div { class: "product-entry-list",
                    for (index, row) in product_rows_snapshot.iter().cloned().enumerate() {
                        {
                            let article_number = index + 1;
                            rsx! {
                                div { key: "{row.id}", class: "product-entry-card",
                                    div { class: "product-entry-heading",
                                        strong { "Artículo {article_number}" }
                                        if product_rows_snapshot.len() > 1 {
                                            button {
                                                class: "remove-entry-button",
                                                r#type: "button",
                                                title: "Retirar estos campos",
                                                onclick: move |_| {
                                                    product_rows.write().remove(index);
                                                },
                                                "×"
                                            }
                                        }
                                    }
                                    div { class: "product-fields product-main-fields",
                                        label { class: "wide-field",
                                            span { "Nombre del producto" }
                                            input {
                                                value: "{row.name}",
                                                placeholder: "Ej. Consola portátil",
                                                oninput: move |event| product_rows.write()[index].name = event.value(),
                                            }
                                        }
                                        label {
                                            span { "Cantidad" }
                                            input {
                                                r#type: "number",
                                                min: "1",
                                                step: "1",
                                                value: "{row.quantity}",
                                                oninput: move |event| product_rows.write()[index].quantity = event.value(),
                                            }
                                        }
                                        label {
                                            span { "Precio unitario" }
                                            input {
                                                inputmode: "decimal",
                                                value: "{row.unit_price}",
                                                placeholder: "0.00",
                                                oninput: move |event| product_rows.write()[index].unit_price = event.value(),
                                            }
                                        }
                                        label {
                                            span { "Envío (informativo; no se suma)" }
                                            input {
                                                inputmode: "decimal",
                                                value: "{row.shipping}",
                                                oninput: move |event| product_rows.write()[index].shipping = event.value(),
                                            }
                                        }
                                        label {
                                            span { "Impuestos" }
                                            input {
                                                inputmode: "decimal",
                                                value: "{row.taxes}",
                                                oninput: move |event| product_rows.write()[index].taxes = event.value(),
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    button {
                        class: "add-product-fields-button",
                        r#type: "button",
                        onclick: move |_| product_rows.write().push(ProductFormRow::empty()),
                        span { "+" }
                        "Añadir otro artículo"
                    }
                }

                div { class: "rate-card",
                    div { class: "rate-card-heading",
                        div {
                            strong { "Tipo de cambio" }
                            span { "1 {currency} → MXN" }
                        }
                        div { class: "rate-mode-toggle",
                            button {
                                class: if *rate_mode.read() == "automatic" { "active" } else { "" },
                                r#type: "button",
                                onclick: move |_| {
                                    rate_mode.set("automatic".to_owned());
                                    rate_snapshot.set(None);
                                },
                                "Automática"
                            }
                            button {
                                class: if *rate_mode.read() == "manual" { "active" } else { "" },
                                r#type: "button",
                                onclick: move |_| {
                                    rate_mode.set("manual".to_owned());
                                    rate_snapshot.set(None);
                                },
                                "Manual"
                            }
                        }
                    }

                    div { class: "rate-base-fields",
                        label {
                            span { "Moneda original de los artículos" }
                            input {
                                maxlength: "3",
                                value: "{currency}",
                                oninput: move |event| {
                                    currency.set(event.value().to_ascii_uppercase());
                                    rate_snapshot.set(None);
                                },
                            }
                        }
                        label {
                            span { "Fecha de la tasa" }
                            input {
                                r#type: "date",
                                value: "{rate_date}",
                                oninput: move |event| {
                                    rate_date.set(event.value());
                                    rate_snapshot.set(None);
                                },
                            }
                        }
                    }

                    if *rate_mode.read() == "automatic" {
                        button {
                            class: "secondary-button rate-button",
                            r#type: "button",
                            disabled: *rate_loading.read(),
                            onclick: move |_| {
                                if *rate_loading.read() {
                                    return;
                                }
                                notice.set(None);
                                panel.write().error = None;
                                let date = match NaiveDate::parse_from_str(&rate_date.read(), "%Y-%m-%d") {
                                    Ok(date) => date,
                                    Err(_) => {
                                        panel.write().error = Some("Selecciona una fecha válida".to_owned());
                                        return;
                                    }
                                };
                                let currency_value = currency.read().clone();
                                rate_loading.set(true);
                                spawn(async move {
                                    let result = tokio::task::spawn_blocking(move || {
                                        fetch_exchange_rate(&currency_value, date)
                                    })
                                    .await
                                    .map_err(|error| error.to_string())
                                    .and_then(|result| result);
                                    match result {
                                        Ok(rate) => {
                                            notice.set(Some(format!(
                                                "Tasa consultada: 1 {} = {} MXN ({})",
                                                rate.currency, rate.rate_to_mxn, rate.rate_date
                                            )));
                                            rate_snapshot.set(Some(rate));
                                        }
                                        Err(error) => panel.write().error = Some(error),
                                    }
                                    rate_loading.set(false);
                                });
                            },
                            if *rate_loading.read() { "Consultando…" } else { "Consultar tasa" }
                        }
                    } else {
                        div { class: "manual-rate-fields",
                            label {
                                span { "Tasa a MXN" }
                                input {
                                    inputmode: "decimal",
                                    value: "{manual_rate}",
                                    placeholder: "0.10688",
                                    oninput: move |event| manual_rate.set(event.value()),
                                }
                            }
                            label {
                                span { "Fuente" }
                                input {
                                    value: "{manual_source}",
                                    placeholder: "Nombre de la fuente",
                                    oninput: move |event| manual_source.set(event.value()),
                                }
                            }
                            label { class: "wide-field",
                                span { "URL o referencia" }
                                input {
                                    value: "{manual_url}",
                                    placeholder: "https://… o documento consultado",
                                    oninput: move |event| manual_url.set(event.value()),
                                }
                            }
                            label { class: "wide-field",
                                span { "Justificación obligatoria" }
                                input {
                                    value: "{manual_reason}",
                                    placeholder: "Explica por qué no se usó la consulta automática",
                                    oninput: move |event| manual_reason.set(event.value()),
                                }
                            }
                            button {
                                class: "secondary-button rate-button wide-field",
                                r#type: "button",
                                onclick: move |_| {
                                    notice.set(None);
                                    panel.write().error = None;
                                    let result = NaiveDate::parse_from_str(&rate_date.read(), "%Y-%m-%d")
                                        .map_err(|_| "Selecciona una fecha válida".to_owned())
                                        .and_then(|date| parse_amount(&manual_rate.read(), "La tasa")
                                            .and_then(|rate| ExchangeRateSnapshot::manual(
                                                currency.read().clone(),
                                                date,
                                                rate,
                                                manual_source.read().clone(),
                                                manual_url.read().clone(),
                                                manual_reason.read().clone(),
                                            ).map_err(|error| error.to_string())));
                                    match result {
                                        Ok(rate) => {
                                            notice.set(Some("Tasa manual preparada con su justificación".to_owned()));
                                            rate_snapshot.set(Some(rate));
                                        }
                                        Err(error) => panel.write().error = Some(error),
                                    }
                                },
                                "Validar tasa manual"
                            }
                        }
                    }

                    if let Some(rate) = rate_snapshot.read().as_ref() {
                        div { class: "rate-result",
                            div {
                                span { class: if rate.is_manual { "rate-badge manual" } else { "rate-badge" }, if rate.is_manual { "MANUAL" } else { "AUTOMÁTICA" } }
                                strong { "1 {rate.currency} = {rate.rate_to_mxn} MXN" }
                            }
                            small { "{rate.rate_date} · {rate.source_name}" }
                            if let Some(reason) = rate.manual_reason.as_ref() {
                                p { "Motivo: {reason}" }
                            }
                        }
                    }
                }

                div { class: "product-form-actions",
                    span { "Los decimales se almacenan exactamente, sin cálculos de punto flotante." }
                    button {
                        class: "primary-button",
                        r#type: "button",
                        onclick: move |_| {
                            notice.set(None);
                            panel.write().error = None;
                            let result = (|| -> Result<Vec<ProductLine>, String> {
                                let rate = rate_snapshot.read().clone()
                                    .ok_or_else(|| "Consulta o valida una tasa antes de guardar".to_owned())?;
                                product_rows.read().iter().enumerate().map(|(index, row)| {
                                    let article = index + 1;
                                    let quantity_value = row.quantity.trim().parse::<u32>()
                                        .map_err(|_| format!("Artículo {article}: la cantidad debe ser un número entero"))?;
                                    let draft = ProductDraft::new(
                                        row.name.clone(),
                                        None,
                                        quantity_value,
                                        parse_amount(&row.unit_price, &format!("Artículo {article}: el precio unitario"))?,
                                        Decimal::ZERO,
                                        parse_amount(&row.shipping, &format!("Artículo {article}: el envío"))?,
                                        parse_amount(&row.taxes, &format!("Artículo {article}: los impuestos"))?,
                                        currency.read().clone(),
                                    ).map_err(|error| format!("Artículo {article}: {error}"))?;
                                    ProductLine::new(case_id, draft, rate.clone())
                                        .map_err(|error| format!("Artículo {article}: {error}"))
                                }).collect()
                            })();
                            match result {
                                Ok(products) => {
                                    let saved = panel.read().store.as_ref()
                                        .ok_or_else(|| "El almacenamiento de productos no está disponible".to_owned())
                                        .and_then(|store| store.add_products(&products).map_err(|error| error.to_string()));
                                    match saved {
                                        Ok(()) => {
                                            let saved_count = products.len();
                                            let saved_total = products.iter().fold(Decimal::ZERO, |total, product| total + product.total_mxn);
                                            notice.set(Some(if saved_count == 1 {
                                                format!("El artículo se agregó con valor de ${} MXN", format_money(saved_total))
                                            } else {
                                                format!("{saved_count} artículos se agregaron con valor conjunto de ${} MXN", format_money(saved_total))
                                            }));
                                            product_rows.set(vec![ProductFormRow::empty()]);
                                            show_product_form.set(false);
                                            panel.write().reload(case_id);
                                            on_changed.call(());
                                        }
                                        Err(error) => panel.write().error = Some(error),
                                    }
                                }
                                Err(error) => panel.write().error = Some(error),
                            }
                        },
                        if product_rows_snapshot.len() == 1 { "Agregar artículo" } else { "Guardar artículos" }
                    }
                }
              }
            }

            if let Some(product) = pending_product {
                div { class: "removal-confirmation",
                    div {
                        strong { "¿Retirar {product.name}?" }
                        p { "El cálculo se eliminará, pero el retiro quedará registrado en la bitácora." }
                    }
                    div {
                        button { class: "text-button", onclick: move |_| pending_removal.set(None), "Cancelar" }
                        button {
                            class: "danger-button",
                            onclick: move |_| {
                                let result = panel.read().store.as_ref()
                                    .ok_or_else(|| "El almacenamiento de productos no está disponible".to_owned())
                                    .and_then(|store| store.remove_product(&product).map_err(|error| error.to_string()));
                                match result {
                                    Ok(()) => {
                                        pending_removal.set(None);
                                        notice.set(Some("Producto retirado correctamente".to_owned()));
                                        panel.write().reload(case_id);
                                        on_changed.call(());
                                    }
                                    Err(error) => panel.write().error = Some(error),
                                }
                            },
                            "Retirar producto"
                        }
                    }
                }
            }

            if snapshot.products.is_empty() {
                div { class: "product-empty",
                    strong { "Aún no hay productos valorados" }
                    span { "Agrega los productos incluidos en tu compra para continuar." }
                    button { class: "primary-button", onclick: move |_| show_product_form.set(true), "+ Agregar producto" }
                }
            } else {
                if !*show_product_form.read() {
                    button { class: "add-product-fields-button product-open-button", onclick: move |_| show_product_form.set(true), "+ Agregar producto" }
                }
                div { class: "product-table",
                    div { class: "product-table-header",
                        span { "Producto" }
                        span { "Valor original" }
                        span { "Tasa guardada" }
                        span { "Total MXN" }
                        span {}
                    }
                    for product in snapshot.products.iter() {
                        {
                            let remove_id = product.id;
                            let value_detail = if product.taxes > Decimal::ZERO {
                                format!(
                                    "Mercancía {} + impuestos {} · envío {} no incluido",
                                    format_money(product.subtotal_original),
                                    format_money(product.taxes),
                                    format_money(product.shipping)
                                )
                            } else {
                                format!(
                                    "Mercancía {} · envío {} no incluido",
                                    format_money(product.subtotal_original),
                                    format_money(product.shipping)
                                )
                            };
                            rsx! {
                                article { key: "{product.id}", class: "product-row",
                                    div {
                                        strong { "{product.name}" }
                                        small { "{product.quantity} × {format_money(product.unit_price)}" }
                                    }
                                    div {
                                        strong { "{format_money(product.total_original)} {product.currency}" }
                                        small { "{value_detail}" }
                                    }
                                    div {
                                        span { class: if product.rate.is_manual { "rate-badge manual" } else { "rate-badge" }, if product.rate.is_manual { "MANUAL" } else { "AUTO" } }
                                        strong { "{product.rate.rate_to_mxn}" }
                                        small { "{product.rate.rate_date} · {product.rate.source_name}" }
                                    }
                                    div { class: "mxn-total", "${format_money(product.total_mxn)}" }
                                    button {
                                        class: "remove-icon-button",
                                        title: "Retirar producto",
                                        onclick: move |_| pending_removal.set(Some(remove_id)),
                                        "×"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn TrackingPanel(case: RectificationCase, on_changed: EventHandler<()>) -> Element {
    let case_id = case.id;
    let tracking_number = case.tracking_number.clone();
    let mut panel = use_signal(move || TrackingPanelState::load(case_id));
    let mut refreshing = use_signal(|| false);
    let mut notice = use_signal(|| None::<String>);
    let mut manual_description = use_signal(String::new);
    let mut manual_location = use_signal(String::new);
    let mut manual_occurred_at = use_signal(|| Local::now().format("%Y-%m-%dT%H:%M").to_string());
    let snapshot = panel.read().clone();
    let unseen_count = snapshot
        .events
        .iter()
        .filter(|event| !event.is_seen)
        .count();
    let last_attempt = snapshot
        .refresh_state
        .last_attempt_at
        .map(|value| {
            value
                .with_timezone(&Local)
                .format("%d/%m/%Y · %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "Sin consultas todavía".to_owned());

    rsx! {
        section { class: "tracking-section",
            div { class: "section-heading",
                div {
                    span { class: "eyebrow", "CORREOS DE MÉXICO" }
                    h4 { "Rastreo y actualizaciones" }
                    p { "Consulta automáticamente cuando han transcurrido 12 horas desde el último intento. Las respuestas crudas quedan guardadas localmente." }
                }
                div { class: "tracking-summary",
                    span { "{snapshot.events.len()} movimientos" }
                    if unseen_count > 0 {
                        strong { "{unseen_count} nuevos" }
                    } else {
                        strong { "Sin novedades" }
                    }
                }
            }

            div { class: "tracking-toolbar",
                div {
                    strong { "Guía {case.tracking_number}" }
                    span { "Último intento: {last_attempt}" }
                }
                button {
                    class: "secondary-button",
                    disabled: *refreshing.read(),
                    onclick: move |_| {
                        if *refreshing.read() {
                            return;
                        }
                        refreshing.set(true);
                        notice.set(None);
                        let tracking_number = tracking_number.clone();
                        spawn(async move {
                            let result = tokio::task::spawn_blocking(move || {
                                refresh_tracking_case(case_id, &tracking_number)
                            })
                            .await
                            .map_err(|error| error.to_string())
                            .and_then(|result| result);
                            match result {
                                Ok(result) if result.inserted_events > 0 => notice.set(Some(format!(
                                    "{} movimiento(s) nuevo(s) encontrado(s)",
                                    result.inserted_events
                                ))),
                                Ok(_) => notice.set(Some("Consulta completada sin movimientos nuevos".to_owned())),
                                Err(error) => panel.write().error = Some(error),
                            }
                            panel.write().reload(case_id);
                            refreshing.set(false);
                            on_changed.call(());
                        });
                    },
                    if *refreshing.read() { "Consultando…" } else { "Actualizar ahora" }
                }
                button {
                    class: "text-button",
                    onclick: move |_| {
                        if let Err(error) = open::that(CorreosMexicoProvider::portal_url()) {
                            panel.write().error = Some(error.to_string());
                        }
                    },
                    "Abrir portal"
                }
                if unseen_count > 0 {
                    button {
                        class: "text-button",
                        onclick: move |_| {
                            let result = panel
                                .read()
                                .store
                                .as_ref()
                                .ok_or_else(|| "El almacenamiento de rastreo no está disponible".to_owned())
                                .and_then(|store| store.mark_tracking_seen(case_id).map_err(|error| error.to_string()));
                            match result {
                                Ok(()) => {
                                    panel.write().reload(case_id);
                                    on_changed.call(());
                                }
                                Err(error) => panel.write().error = Some(error),
                            }
                        },
                        "Marcar como revisadas"
                    }
                }
            }

            if let Some(message) = notice.read().as_ref() {
                div { class: "alert success", "{message}" }
            }
            if let Some(error) = snapshot.refresh_state.last_error.as_ref().or(snapshot.error.as_ref()) {
                div { class: "tracking-error",
                    strong { "Consulta automática no disponible" }
                    span { "{error}" }
                    small { "Puedes abrir el portal oficial o registrar el movimiento manualmente." }
                }
            }

            if snapshot.events.is_empty() {
                div { class: "tracking-empty",
                    strong { "Todavía no hay movimientos guardados" }
                    span { "La ausencia de eventos no se interpreta como entrega ni como incidencia." }
                }
            } else {
                div { class: "tracking-timeline",
                    for event in snapshot.events.iter() {
                        {
                            let occurred_at = event
                                .occurred_at
                                .map(|value| value.format("%d/%m/%Y · %H:%M").to_string())
                                .unwrap_or_else(|| "Fecha no informada".to_owned());
                            rsx! {
                                article { key: "{event.id}", class: if event.is_seen { "tracking-event" } else { "tracking-event unseen" },
                                    span { class: "tracking-event-dot" }
                                    div {
                                        strong { "{event.description}" }
                                        if let Some(location) = event.location.as_ref() {
                                            span { "{location}" }
                                        }
                                        small { "{occurred_at} · {tracking_source_label(&event.source)}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            details { class: "manual-tracking",
                summary { "Registrar actualización manual" }
                div { class: "manual-tracking-fields",
                    label { class: "wide-field",
                        span { "Descripción del movimiento" }
                        input {
                            value: "{manual_description}",
                            placeholder: "Ej. En tránsito hacia oficina de destino",
                            oninput: move |event| manual_description.set(event.value()),
                        }
                    }
                    label {
                        span { "Fecha y hora" }
                        input {
                            r#type: "datetime-local",
                            value: "{manual_occurred_at}",
                            oninput: move |event| manual_occurred_at.set(event.value()),
                        }
                    }
                    label {
                        span { "Ubicación" }
                        input {
                            value: "{manual_location}",
                            placeholder: "Opcional",
                            oninput: move |event| manual_location.set(event.value()),
                        }
                    }
                    button {
                        class: "secondary-button wide-field",
                        onclick: move |_| {
                            let result = (|| -> Result<TrackingUpdateResult, String> {
                                let description = manual_description.read().trim().to_owned();
                                if description.is_empty() {
                                    return Err("Describe el movimiento que deseas registrar".to_owned());
                                }
                                let occurred_at = if manual_occurred_at.read().trim().is_empty() {
                                    None
                                } else {
                                    Some(
                                        NaiveDateTime::parse_from_str(
                                            manual_occurred_at.read().trim(),
                                            "%Y-%m-%dT%H:%M",
                                        )
                                        .map_err(|_| "La fecha manual no es válida".to_owned())?
                                        .and_utc(),
                                    )
                                };
                                let event = TrackingEventInput {
                                    occurred_at,
                                    description,
                                    location: Some(manual_location.read().clone())
                                        .filter(|value| !value.trim().is_empty()),
                                };
                                panel
                                    .read()
                                    .store
                                    .as_ref()
                                    .ok_or_else(|| "El almacenamiento de rastreo no está disponible".to_owned())?
                                    .add_manual_tracking_event(case_id, event)
                                    .map_err(|error| error.to_string())
                            })();
                            match result {
                                Ok(result) if result.inserted_events > 0 => {
                                    manual_description.set(String::new());
                                    manual_location.set(String::new());
                                    notice.set(Some("Movimiento manual agregado".to_owned()));
                                    panel.write().reload(case_id);
                                    on_changed.call(());
                                }
                                Ok(_) => notice.set(Some("Ese movimiento ya estaba registrado".to_owned())),
                                Err(error) => panel.write().error = Some(error),
                            }
                        },
                        "Guardar movimiento manual"
                    }
                }
            }
        }
    }
}

#[component]
fn DocumentPanel(
    case: RectificationCase,
    applicant_profile: ApplicantProfile,
    on_changed: EventHandler<()>,
) -> Element {
    let case_id = case.id;
    let export_case = case.clone();
    let bundle_case = case.clone();
    let initial_customs_valuation = CaseDataStore::open_default()
        .and_then(|store| store.load_customs_valuation(case_id))
        .ok()
        .flatten()
        .map(|value| value.normalize().to_string())
        .unwrap_or_default();
    let ApplicantProfile {
        full_name: profile_name,
        email: profile_email,
        phone: profile_phone,
        address: profile_address,
        city: profile_city,
        state: profile_state,
        postal_code: profile_postal_code,
    } = applicant_profile;
    let mut full_name = use_signal(move || profile_name);
    let mut email = use_signal(move || profile_email);
    let mut phone = use_signal(move || profile_phone);
    let mut address = use_signal(move || profile_address);
    let mut authority_name = use_signal(|| "Autoridad aduanera competente".to_owned());
    let mut authority_email = use_signal(|| DEFAULT_CUSTOMS_EMAIL.to_owned());
    let mut presumptive_value_mxn = use_signal(move || initial_customs_valuation);
    let mut valuation_save_status = use_signal(|| None::<Result<(), String>>);
    let mut city = use_signal(move || profile_city);
    let mut state = use_signal(move || profile_state);
    let mut postal_code = use_signal(move || profile_postal_code);
    let mut issuance_date = use_signal(current_date_in_spanish);
    let mut non_commercial_statement = use_signal(|| {
        "La mercancía corresponde a artículos en cantidad razonable para uso personal. No existe habitualidad, volumen comercial ni finalidad lucrativa.".to_owned()
    });
    let mut request_notes = use_signal(String::new);
    let mut last_bundle = use_signal(|| None::<GeneratedBundle>);
    let mut show_export_dialog = use_signal(|| false);
    let mut export_format = use_signal(|| "pdf".to_owned());
    let mut export_loading = use_signal(|| false);
    let mut export_dialog_error = use_signal(|| None::<String>);
    let mut export_success = use_signal(|| None::<String>);
    let mut error = use_signal(|| None::<String>);
    let mut notice = use_signal(|| None::<String>);
    let bundle_snapshot = last_bundle.read().clone();
    let real_value_mxn = CaseDataStore::open_default()
        .and_then(|store| store.list_products(case_id))
        .map(|products| {
            products
                .iter()
                .fold(Decimal::ZERO, |total, product| total + product.total_mxn)
        })
        .unwrap_or(Decimal::ZERO);
    let customs_value = parse_amount(
        presumptive_value_mxn.read().as_str(),
        "La valuación asignada por Aduanas",
    )
    .ok();
    let overvaluation =
        customs_value.and_then(|value| calculate_customs_overvaluation(value, real_value_mxn));

    rsx! {
        section { class: "document-section",
            div { class: "section-heading",
                div {
                    span { class: "eyebrow", "GENERADOR DOCUMENTAL" }
                    h4 { "Solicitud y dossier de pruebas" }
                    p { "Genera un PDF consolidado para imprimir, un Word editable y el paquete documental completo." }
                }
                span { class: "template-badge", "FORMATO ADUANERO" }
            }

            div { class: "plaintext-warning",
                strong { "Salida sin cifrar" }
                span { "Los documentos generados se guardarán en la carpeta que elijas para que puedas revisarlos y enviarlos." }
            }

            div { class: "plaintext-warning",
                strong { "Cálculo postal" }
                span { "Al generar, la app consulta una referencia USD/MXN para evaluar el umbral de 50 USD y calcular 19% cuando corresponda. Confirma el tipo de cambio aduanero antes de enviar." }
            }

            if let Some(message) = notice.read().as_ref() {
                div { class: "alert success", "{message}" }
            }
            if let Some(message) = error.read().as_ref() {
                div { class: "alert error", "{message}" }
            }

            div { class: "document-form",
                div { class: "document-fields",
                    label {
                        span { "Nombre completo" }
                        input { value: "{full_name}", placeholder: "Persona solicitante", oninput: move |event| full_name.set(event.value()) }
                    }
                    label {
                        span { "Correo del solicitante" }
                        input { r#type: "email", value: "{email}", placeholder: "nombre@correo.com", oninput: move |event| email.set(event.value()) }
                    }
                    label {
                        span { "Teléfono" }
                        input { value: "{phone}", placeholder: "Opcional", oninput: move |event| phone.set(event.value()) }
                    }
                    label {
                        span { "Domicilio" }
                        input { value: "{address}", placeholder: "Opcional", oninput: move |event| address.set(event.value()) }
                    }
                    label {
                        span { "Autoridad destinataria" }
                        input { value: "{authority_name}", oninput: move |event| authority_name.set(event.value()) }
                    }
                    label {
                        span { "Correo de la autoridad" }
                        input { r#type: "email", value: "{authority_email}", placeholder: "Confirma el destinatario oficial", oninput: move |event| authority_email.set(event.value()) }
                    }
                    label { class: "wide-field customs-valuation-field",
                        span { "Valuación incorrecta asignada por Aduanas (MXN)" }
                        input {
                            value: "{presumptive_value_mxn}",
                            inputmode: "decimal",
                            placeholder: "Ej. 6055.87",
                            oninput: move |event| {
                                let value = event.value();
                                presumptive_value_mxn.set(value.clone());
                                let parsed = parse_amount(&value, "La valuación asignada por Aduanas");
                                match parsed {
                                    Ok(amount) if amount > Decimal::ZERO => {
                                        let result = CaseDataStore::open_default()
                                            .and_then(|store| store.save_customs_valuation(case_id, amount))
                                            .map_err(|error| error.to_string());
                                        valuation_save_status.set(Some(result));
                                    }
                                    _ => valuation_save_status.set(None),
                                }
                            }
                        }
                        small { "Captura el valor total que Aduanas asentó en la boleta, no el impuesto cobrado." }
                        if let Some(status) = valuation_save_status.read().as_ref() {
                            small { class: if status.is_ok() { "autosave-status saved" } else { "autosave-status error" },
                                if let Err(message) = status { "No se pudo guardar: {message}" } else { "✓ Guardado automáticamente en este expediente" }
                            }
                        }
                    }
                    if real_value_mxn > Decimal::ZERO {
                        div { class: "valuation-comparison wide-field",
                            div {
                                span { "Valor real comprobado" }
                                strong { "${format_money(real_value_mxn)} MXN" }
                            }
                            if let Some(comparison) = overvaluation {
                                div { class: "valuation-excess",
                                    span { "Exceso de valuación" }
                                    strong { "+{comparison.percentage_above_real_value:.2}%" }
                                    small { "Aduanas agregó ${comparison.difference_mxn:.2} MXN sobre el valor real." }
                                }
                            } else if customs_value.is_some() {
                                div {
                                    span { "Comparación" }
                                    strong { "Sin exceso positivo" }
                                    small { "La valuación capturada no supera el valor real comprobado." }
                                }
                            } else {
                                div {
                                    span { "Comparación automática" }
                                    strong { "Captura la valuación aduanera" }
                                    small { "El porcentaje se calculará sin incluir envío ni descuentos." }
                                }
                            }
                        }
                    }
                    label {
                        span { "Ciudad del solicitante" }
                        input { value: "{city}", placeholder: "Ej. Puebla", oninput: move |event| city.set(event.value()) }
                    }
                    label {
                        span { "Estado del solicitante" }
                        input { value: "{state}", placeholder: "Ej. Puebla", oninput: move |event| state.set(event.value()) }
                    }
                    label {
                        span { "Código postal" }
                        input { value: "{postal_code}", inputmode: "numeric", maxlength: "5", placeholder: "Ej. 72000", oninput: move |event| postal_code.set(event.value()) }
                    }
                    label {
                        span { "Fecha del escrito" }
                        input { value: "{issuance_date}", placeholder: "Ej. 14 de agosto de 2026", oninput: move |event| issuance_date.set(event.value()) }
                    }
                    label { class: "wide-field",
                        span { "Naturaleza no comercial" }
                        textarea { value: "{non_commercial_statement}", oninput: move |event| non_commercial_statement.set(event.value()) }
                    }
                    label { class: "wide-field",
                        span { "Notas para la solicitud" }
                        textarea { value: "{request_notes}", placeholder: "Hechos relevantes o explicación adicional", oninput: move |event| request_notes.set(event.value()) }
                    }
                }
                div { class: "document-actions",
                    div {
                        strong { "Revisión humana obligatoria" }
                        span { "La app prepara borradores; no envía nada automáticamente." }
                    }
                    button {
                        class: "primary-button",
                        onclick: move |_| {
                            error.set(None);
                            notice.set(None);
                            export_dialog_error.set(None);
                            export_success.set(None);
                            export_format.set("pdf".to_owned());
                            show_export_dialog.set(true);
                        },
                        "Exportar expediente"
                    }
                    button {
                        hidden: !SHOW_EMAIL_WORKFLOW,
                        class: "secondary-button",
                        onclick: move |_| {
                            error.set(None);
                            notice.set(None);
                            let Some(destination) = rfd::FileDialog::new().pick_folder() else { return; };
                            let result = (|| -> Result<GeneratedBundle, String> {
                                let store = CaseDataStore::open_default().map_err(|value| value.to_string())?;
                                let products = store.list_products(case_id).map_err(|value| value.to_string())?;
                                let rate_date = products.iter()
                                    .map(|product| product.rate.rate_date)
                                    .max()
                                    .ok_or_else(|| "Agrega al menos un producto valorado antes de generar".to_owned())?;
                                let usd_rate = products.iter()
                                    .find(|product| product.currency == "USD" && product.rate.rate_date == rate_date)
                                    .map(|product| product.rate.clone())
                                    .map(Ok)
                                    .unwrap_or_else(|| {
                                        std::thread::spawn(move || fetch_exchange_rate("USD", rate_date))
                                            .join()
                                            .map_err(|_| "La consulta USD/MXN terminó inesperadamente".to_owned())
                                            .and_then(|result| result)
                                    })?;
                                let applicant = ApplicantDetails {
                                    full_name: full_name.read().clone(),
                                    email: email.read().clone(),
                                    phone: phone.read().clone(),
                                    address: address.read().clone(),
                                    authority_name: authority_name.read().clone(),
                                    authority_email: authority_email.read().clone(),
                                    presumptive_value_mxn: presumptive_value_mxn.read().clone(),
                                    city: city.read().clone(),
                                    state: state.read().clone(),
                                    postal_code: postal_code.read().clone(),
                                    issuance_date: issuance_date.read().clone(),
                                    non_commercial_statement: non_commercial_statement.read().clone(),
                                    request_notes: request_notes.read().clone(),
                                    usd_rate,
                                };
                                let vault = EvidenceVault::open_default().map_err(|value| value.to_string())?;
                                let evidence = vault.list_evidence(case_id).map_err(|value| value.to_string())?
                                    .into_iter()
                                    .map(|document| {
                                        vault.load_evidence_bytes(&document)
                                            .map(|bytes| EvidenceAsset { document, bytes })
                                            .map_err(|value| value.to_string())
                                    })
                                    .collect::<Result<Vec<_>, _>>()?;
                                let bundle = generate_bundle(&bundle_case, &applicant, &products, &evidence, &destination)
                                    .map_err(|value| value.to_string())?;
                                store.record_document_generation(case_id).map_err(|value| value.to_string())?;
                                let prepared_at = Utc::now();
                                store.save_email_draft(&EmailDraft {
                                    case_id,
                                    recipient: bundle.email_content.recipient.clone(),
                                    sender: bundle.email_content.sender.clone(),
                                    subject: bundle.email_content.subject.clone(),
                                    body: bundle.email_content.body.clone(),
                                    request_pdf_path: bundle.request_pdf.to_string_lossy().into_owned(),
                                    evidence_pdf_path: bundle.evidence_pdf.to_string_lossy().into_owned(),
                                    eml_path: bundle.email_draft.to_string_lossy().into_owned(),
                                    prepared_at,
                                    opened_at: None,
                                    sent_at: None,
                                }).map_err(|value| value.to_string())?;
                                Ok(bundle)
                            })();
                            match result {
                                Ok(bundle) => {
                                    notice.set(Some(format!("Paquete generado en {}", bundle.directory.display())));
                                    last_bundle.set(Some(bundle));
                                    on_changed.call(());
                                }
                                Err(message) => error.set(Some(message)),
                            }
                        },
                        if bundle_snapshot.is_some() { "Regenerar paquete para correo" } else { "Preparar paquete para correo" }
                    }
                }
            }

            if *show_export_dialog.read() {
                div { class: "modal-backdrop", onclick: move |_| if !*export_loading.read() { show_export_dialog.set(false); },
                    article { class: "export-format-dialog", onclick: move |event| event.stop_propagation(),
                        span { class: "eyebrow", "EXPORTAR EXPEDIENTE" }
                        h4 { "¿En qué formato deseas guardarlo?" }
                        p { "Después elegirás el nombre y la ubicación del archivo." }
                        div { class: "export-format-options",
                            button {
                                class: if export_format.read().as_str() == "pdf" { "selected" } else { "" },
                                onclick: move |_| export_format.set("pdf".to_owned()),
                                span { "PDF" }
                                div { strong { "Listo para imprimir" } small { "Incluye el escrito y todas las pruebas adjuntas." } }
                            }
                            button {
                                class: if export_format.read().as_str() == "docx" { "selected" } else { "" },
                                onclick: move |_| export_format.set("docx".to_owned()),
                                span { "DOCX" }
                                div { strong { "Word editable" } small { "Permite ajustar el escrito antes de imprimirlo." } }
                            }
                        }
                        if let Some(message) = export_dialog_error.read().as_ref() {
                            div { class: "alert error export-dialog-error", "{message}" }
                        }
                        div { class: "export-dialog-actions",
                            button { class: "text-button", disabled: *export_loading.read(), onclick: move |_| show_export_dialog.set(false), "Cancelar" }
                            button {
                                class: "primary-button",
                                disabled: *export_loading.read(),
                                onclick: move |_| {
                                    if *export_loading.read() {
                                        return;
                                    }
                                    export_dialog_error.set(None);
                                    let selected_format = export_format.read().clone();
                                    let extension = if selected_format == "pdf" { "pdf" } else { "docx" };
                                    let format_label = if selected_format == "pdf" { "PDF" } else { "DOCX" };
                                    let filename = format!("expediente-{}.{}", export_case.tracking_number, extension);
                                    let Some(mut destination) = rfd::FileDialog::new()
                                        .add_filter(format_label, &[extension])
                                        .set_file_name(&filename)
                                        .save_file()
                                    else {
                                        return;
                                    };
                                    if destination.extension().and_then(|value| value.to_str()) != Some(extension) {
                                        destination.set_extension(extension);
                                    }
                                    let input = DocumentApplicantInput {
                                        full_name: full_name.read().clone(),
                                        email: email.read().clone(),
                                        phone: phone.read().clone(),
                                        address: address.read().clone(),
                                        authority_name: authority_name.read().clone(),
                                        authority_email: authority_email.read().clone(),
                                        presumptive_value_mxn: presumptive_value_mxn.read().clone(),
                                        city: city.read().clone(),
                                        state: state.read().clone(),
                                        postal_code: postal_code.read().clone(),
                                        issuance_date: issuance_date.read().clone(),
                                        non_commercial_statement: non_commercial_statement.read().clone(),
                                        request_notes: request_notes.read().clone(),
                                    };
                                    let case_for_export = export_case.clone();
                                    export_loading.set(true);
                                    spawn(async move {
                                        let result = tokio::task::spawn_blocking(move || {
                                            export_case_document(
                                                &case_for_export,
                                                input,
                                                &selected_format,
                                                &destination,
                                            )
                                        })
                                        .await
                                        .map_err(|_| "La exportación terminó inesperadamente".to_owned())
                                        .and_then(|result| result);
                                        export_loading.set(false);
                                        match result {
                                            Ok(path) => {
                                                show_export_dialog.set(false);
                                                let message = format!("Expediente {} guardado correctamente en {}", format_label, path.display());
                                                notice.set(Some(message.clone()));
                                                export_success.set(Some(message));
                                                on_changed.call(());
                                            }
                                            Err(message) => export_dialog_error.set(Some(message)),
                                        }
                                    });
                                },
                                if *export_loading.read() { "Exportando…" } else { "Continuar" }
                            }
                        }
                    }
                }
            }

            if let Some(message) = export_success.read().as_ref() {
                div { class: "export-success-toast",
                    div {
                        span { "✓" }
                        div { strong { "Exportación exitosa" } small { "{message}" } }
                    }
                    button { aria_label: "Cerrar notificación", onclick: move |_| export_success.set(None), "×" }
                }
            }

            if let Some(bundle) = bundle_snapshot {
                div { class: "generated-files",
                    div { class: "generated-file featured",
                        span { "PDF" }
                        div { strong { "Expediente listo para imprimir" } small { "03_expediente_listo_para_imprimir.pdf" } }
                        button { class: "secondary-button", onclick: move |_| if let Err(value) = open::that(&bundle.print_pdf) { error.set(Some(value.to_string())); }, "Abrir" }
                    }
                    div { class: "generated-file featured",
                        span { "DOCX" }
                        div { strong { "Solicitud editable en Word" } small { "04_solicitud_rectificacion_editable.docx" } }
                        button { class: "secondary-button", onclick: move |_| if let Err(value) = open::that(&bundle.request_docx) { error.set(Some(value.to_string())); }, "Abrir" }
                    }
                    div { class: "generated-file",
                        span { "PDF" }
                        div { strong { "Solicitud de rectificación" } small { "01_solicitud_rectificacion.pdf" } }
                        button { class: "secondary-button", onclick: move |_| if let Err(value) = open::that(&bundle.request_pdf) { error.set(Some(value.to_string())); }, "Abrir" }
                    }
                    div { class: "generated-file",
                        span { "PDF" }
                        div { strong { "Dossier de pruebas" } small { "02_dossier_pruebas.pdf" } }
                        button { class: "secondary-button", onclick: move |_| if let Err(value) = open::that(&bundle.evidence_pdf) { error.set(Some(value.to_string())); }, "Abrir" }
                    }
                    div { class: "generated-file",
                        span { "ZIP" }
                        div { strong { "Expediente completo" } small { "Incluye PDF, Word, originales, manifiesto y correo .eml" } }
                        button { class: "secondary-button", onclick: move |_| if let Err(value) = open::that(&bundle.directory) { error.set(Some(value.to_string())); }, "Ver carpeta" }
                    }
                }
            }
        }
    }
}

#[component]
fn EmailPanel(
    case: RectificationCase,
    applicant_profile: ApplicantProfile,
    on_changed: EventHandler<()>,
) -> Element {
    let case_id = case.id;
    let initial = EmailPanelState::load(case_id);
    let initial_draft = initial.draft.clone();
    let mut recipient = use_signal(|| {
        initial_draft
            .as_ref()
            .map(|draft| draft.recipient.clone())
            .unwrap_or_else(|| DEFAULT_CUSTOMS_EMAIL.to_owned())
    });
    let initial_draft = initial.draft.clone();
    let profile_email = applicant_profile.email.clone();
    let mut sender = use_signal(move || {
        initial_draft
            .as_ref()
            .map(|draft| draft.sender.clone())
            .unwrap_or(profile_email)
    });
    let initial_draft = initial.draft.clone();
    let tracking_number = case.tracking_number.clone();
    let mut subject = use_signal(move || {
        initial_draft
            .as_ref()
            .map(|draft| draft.subject.clone())
            .unwrap_or_else(|| format!("Solicitud de rectificación - guía {tracking_number}"))
    });
    let initial_draft = initial.draft.clone();
    let mut body = use_signal(move || {
        initial_draft
            .as_ref()
            .map(|draft| draft.body.clone())
            .unwrap_or_default()
    });
    let mut panel = use_signal(move || initial);
    let mut notice = use_signal(|| None::<String>);
    let mut confirm_sent = use_signal(|| false);
    let snapshot = panel.read().clone();
    let prepared_label = snapshot.draft.as_ref().map(|draft| {
        draft
            .prepared_at
            .with_timezone(&Local)
            .format("%d/%m/%Y · %H:%M")
            .to_string()
    });
    let sent_label = snapshot.draft.as_ref().and_then(|draft| {
        draft.sent_at.map(|value| {
            value
                .with_timezone(&Local)
                .format("%d/%m/%Y · %H:%M")
                .to_string()
        })
    });

    rsx! {
        section { class: "email-section",
            div { class: "section-heading",
                div {
                    span { class: "eyebrow", "REVISIÓN Y ENVÍO MANUAL" }
                    h4 { "Correo para Aduanas" }
                    p { "Revisa cada dato y los adjuntos antes de abrir el borrador en tu cliente de correo." }
                }
                span { class: if sent_label.is_some() { "email-status sent" } else if snapshot.draft.is_some() { "email-status ready" } else { "email-status" },
                    if sent_label.is_some() { "ENVIADO" } else if snapshot.draft.is_some() { "PREPARADO" } else { "PENDIENTE" }
                }
            }

            div { class: "transmission-warning",
                strong { "Sin envío automático" }
                span { "Abrir el archivo .eml no confirma ni registra el envío. Tú conservas el control final desde tu cliente de correo." }
            }

            if let Some(message) = notice.read().as_ref() {
                div { class: "alert success", "{message}" }
            }
            if let Some(message) = snapshot.error.as_ref() {
                div { class: "alert error", "{message}" }
            }

            if snapshot.draft.is_none() {
                div { class: "email-empty",
                    strong { "Primero genera los documentos" }
                    span { "El borrador se habilitará cuando existan la solicitud y el dossier de pruebas." }
                }
            }

            div { class: "email-form",
                div { class: "email-fields",
                    label {
                        span { "Para" }
                        input { r#type: "email", value: "{recipient}", placeholder: "Confirma el correo oficial de la autoridad", oninput: move |event| recipient.set(event.value()) }
                    }
                    label {
                        span { "De" }
                        input { r#type: "email", value: "{sender}", placeholder: "Correo del solicitante", oninput: move |event| sender.set(event.value()) }
                    }
                    label { class: "wide-field",
                        span { "Asunto" }
                        input { value: "{subject}", oninput: move |event| subject.set(event.value()) }
                    }
                    label { class: "wide-field",
                        span { "Cuerpo del mensaje" }
                        textarea { value: "{body}", placeholder: "Se generará junto con los documentos", oninput: move |event| body.set(event.value()) }
                    }
                }

                div { class: "email-attachments",
                    strong { "Adjuntos incluidos" }
                    div {
                        span { class: "attachment-pill", "PDF · Solicitud de rectificación" }
                        span { class: "attachment-pill", "PDF · Dossier con todas las pruebas" }
                    }
                }

                if let Some(prepared) = prepared_label.as_ref() {
                    div { class: "email-timestamps",
                        span { "Preparado: {prepared}" }
                        if let Some(sent) = sent_label.as_ref() {
                            span { "Marcado como enviado: {sent}" }
                        }
                    }
                }

                div { class: "email-actions",
                    button {
                        class: "secondary-button",
                        disabled: snapshot.draft.is_none(),
                        onclick: move |_| {
                            notice.set(None);
                            let result = (|| -> Result<(), String> {
                                let current = panel.read().draft.clone()
                                    .ok_or_else(|| "Genera los documentos antes de preparar el correo".to_owned())?;
                                let content = EmailContent {
                                    recipient: recipient.read().trim().to_owned(),
                                    sender: sender.read().trim().to_owned(),
                                    subject: subject.read().trim().to_owned(),
                                    body: body.read().trim().to_owned(),
                                };
                                write_email_draft(
                                    Path::new(&current.eml_path),
                                    &content,
                                    Path::new(&current.request_pdf_path),
                                    Path::new(&current.evidence_pdf_path),
                                ).map_err(|value| value.to_string())?;
                                let updated = EmailDraft {
                                    recipient: content.recipient,
                                    sender: content.sender,
                                    subject: content.subject,
                                    body: content.body,
                                    prepared_at: Utc::now(),
                                    opened_at: None,
                                    sent_at: None,
                                    ..current
                                };
                                panel.read().store.as_ref()
                                    .ok_or_else(|| "El almacenamiento de correo no está disponible".to_owned())?
                                    .save_email_draft(&updated)
                                    .map_err(|value| value.to_string())
                            })();
                            match result {
                                Ok(()) => {
                                    panel.write().reload(case_id);
                                    notice.set(Some("Borrador y adjuntos actualizados".to_owned()));
                                    confirm_sent.set(false);
                                    on_changed.call(());
                                }
                                Err(message) => panel.write().error = Some(message),
                            }
                        },
                        "Guardar borrador"
                    }
                    button {
                        class: "primary-button",
                        disabled: snapshot.draft.is_none(),
                        onclick: move |_| {
                            notice.set(None);
                            let result = panel.read().draft.as_ref()
                                .ok_or_else(|| "Prepara el borrador antes de abrirlo".to_owned())
                                .and_then(|draft| open::that(&draft.eml_path).map_err(|value| value.to_string()))
                                .and_then(|()| panel.read().store.as_ref()
                                    .ok_or_else(|| "El almacenamiento de correo no está disponible".to_owned())?
                                    .record_email_opened(case_id)
                                    .map_err(|value| value.to_string()));
                            match result {
                                Ok(()) => {
                                    panel.write().reload(case_id);
                                    notice.set(Some("Borrador abierto. Revisa los destinatarios y adjuntos en tu cliente de correo.".to_owned()));
                                    on_changed.call(());
                                }
                                Err(message) => panel.write().error = Some(message),
                            }
                        },
                        "Abrir en cliente de correo"
                    }
                    if snapshot.draft.is_some() && sent_label.is_none() {
                        button { class: "text-button", onclick: move |_| confirm_sent.set(true), "Marcar como enviado" }
                    }
                }

                if *confirm_sent.read() {
                    div { class: "sent-confirmation",
                        div {
                            strong { "¿Confirmas que ya lo enviaste?" }
                            span { "La app no puede comprobar el envío; solo registrará tu confirmación manual." }
                        }
                        div {
                            button { class: "text-button", onclick: move |_| confirm_sent.set(false), "Cancelar" }
                            button {
                                class: "primary-button",
                                onclick: move |_| {
                                    let result = panel.read().store.as_ref()
                                        .ok_or_else(|| "El almacenamiento de correo no está disponible".to_owned())
                                        .and_then(|store| store.mark_email_sent(case_id).map_err(|value| value.to_string()));
                                    match result {
                                        Ok(()) => {
                                            confirm_sent.set(false);
                                            panel.write().reload(case_id);
                                            notice.set(Some("El expediente se marcó como enviado".to_owned()));
                                            on_changed.call(());
                                        }
                                        Err(message) => panel.write().error = Some(message),
                                    }
                                },
                                "Sí, marcar enviado"
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn CaptureEvidenceStep(
    case_id: Uuid,
    kind: EvidenceKind,
    title: &'static str,
    description: &'static str,
    on_changed: EventHandler<()>,
) -> Element {
    let mut panel = use_signal(move || EvidencePanelState::load(case_id));
    let mut pending_removal = use_signal(|| None::<EvidenceDocument>);
    let snapshot = panel.read().clone();
    let documents = snapshot
        .documents
        .iter()
        .filter(|document| document.kind == kind)
        .cloned()
        .collect::<Vec<_>>();
    let document_count = documents.len();

    rsx! {
        section { class: "capture-section",
            div { class: "capture-section-heading",
                div {
                    span { class: "eyebrow", if kind == EvidenceKind::BankStatement { "PAGOS" } else { "PRUEBAS DEL VALOR" } }
                    h3 { "{title}" }
                    p { "{description}" }
                }
                span { "{document_count} archivos" }
            }

            if let Some(error) = snapshot.error.as_ref() {
                div { class: "alert error", "{error}" }
            }

            if documents.is_empty() {
                div { class: "capture-upload-empty",
                    strong { if kind == EvidenceKind::BankStatement { "Todavía no has agregado documentos de pago" } else { "Todavía no has agregado comprobantes" } }
                    p { if kind == EvidenceKind::BankStatement { "Puedes agregar estados de cuenta en PDF, capturas bancarias o comprobantes de transferencia." } else { "Agrega facturas o recibos en PDF, además de capturas o imágenes que acrediten el precio." } }
                    button {
                        class: "primary-button",
                        onclick: move |_| {
                            let Some(path) = rfd::FileDialog::new()
                                .add_filter("PDF e imágenes", &["pdf", "png", "jpg", "jpeg", "webp"])
                                .pick_file() else { return; };
                            let result = panel.read().vault.as_ref()
                                .ok_or_else(|| "El almacén cifrado no está disponible".to_owned())
                                .and_then(|vault| vault.import_evidence(case_id, kind, None, &path).map_err(|error| error.to_string()));
                            match result {
                                Ok(_) => { panel.write().reload(case_id); on_changed.call(()); }
                                Err(error) => panel.write().error = Some(error),
                            }
                        },
                        "+ Agregar PDF o imagen"
                    }
                }
            } else {
                div { class: "capture-document-list",
                    for document in documents.iter() {
                        {
                            let remove_document = document.clone();
                            let size_kb = document.size_bytes.div_ceil(1024);
                            rsx! {
                                div { key: "{document.id}", class: "capture-document-row",
                                    div {
                                        strong { "{document.original_filename}" }
                                        span { "{document.kind.label()} · {size_kb} KB · ✓ Cargado" }
                                    }
                                    button { class: "text-button", onclick: move |_| pending_removal.set(Some(remove_document.clone())), "Eliminar" }
                                }
                            }
                        }
                    }
                    button {
                        class: "add-product-fields-button",
                        onclick: move |_| {
                            let Some(path) = rfd::FileDialog::new()
                                .add_filter("PDF e imágenes", &["pdf", "png", "jpg", "jpeg", "webp"])
                                .pick_file() else { return; };
                            let result = panel.read().vault.as_ref()
                                .ok_or_else(|| "El almacén cifrado no está disponible".to_owned())
                                .and_then(|vault| vault.import_evidence(case_id, kind, None, &path).map_err(|error| error.to_string()));
                            match result {
                                Ok(_) => { panel.write().reload(case_id); on_changed.call(()); }
                                Err(error) => panel.write().error = Some(error),
                            }
                        },
                        "+ Agregar otro PDF o imagen"
                    }
                }
            }

            if let Some(document) = pending_removal.read().clone() {
                div { class: "removal-confirmation",
                    div { strong { "¿Eliminar {document.original_filename}?" } p { "Este documento será retirado de la rectificación." } }
                    div {
                        button { class: "text-button", onclick: move |_| pending_removal.set(None), "Cancelar" }
                        button { class: "danger-button", onclick: move |_| {
                            let result = panel.read().vault.as_ref()
                                .ok_or_else(|| "El almacén cifrado no está disponible".to_owned())
                                .and_then(|vault| vault.remove_evidence(&document).map_err(|error| error.to_string()));
                            match result {
                                Ok(()) => { pending_removal.set(None); panel.write().reload(case_id); on_changed.call(()); }
                                Err(error) => panel.write().error = Some(error),
                            }
                        }, "Eliminar" }
                    }
                }
            }
        }
    }
}

#[component]
fn CaptureCaseWizard(
    case: RectificationCase,
    applicant_profile: ApplicantProfile,
    step: usize,
    on_step_changed: EventHandler<usize>,
    on_finished: EventHandler<()>,
    on_case_changed: EventHandler<()>,
) -> Element {
    let case_id = case.id;
    let products = CaseDataStore::open_default()
        .and_then(|store| store.list_products(case_id))
        .unwrap_or_default();
    let evidence = EvidenceVault::open_default()
        .and_then(|vault| vault.list_evidence(case_id))
        .unwrap_or_default();
    let value_count = evidence
        .iter()
        .filter(|item| item.kind == EvidenceKind::Product)
        .count();
    let payment_count = evidence
        .iter()
        .filter(|item| item.kind == EvidenceKind::BankStatement)
        .count();
    let product_count = products.len();
    let can_finish = !products.is_empty() && value_count > 0;

    rsx! {
        section { class: "capture-wizard",
            div { class: "capture-progress",
                for (index, number, label) in [(0_usize, "1", "General"), (1, "2", "Envío"), (2, "3", "Productos"), (3, "4", "Comprobantes"), (4, "5", "Pagos"), (5, "6", "Revisión")] {
                    div { class: if index < step { "done" } else if index == step { "active" } else { "" },
                        span { if index < step { "✓" } else { "{number}" } }
                        strong { "{label}" }
                        small { if index < step { "Completo" } else if index == step { "En progreso" } else { "Pendiente" } }
                    }
                }
            }

            if step == 2 {
                div { class: "capture-intro", span { class: "eyebrow", "PASO 3 DE 6" } h2 { "Productos y costos" } p { "Agrega cada artículo de la compra y conserva siempre visibles su moneda original y equivalencia en MXN." } }
                ProductPanel { case_id, on_changed: move |_| on_case_changed.call(()) }
            } else if step == 3 {
                CaptureEvidenceStep { case_id, kind: EvidenceKind::Product, title: "Comprobantes del valor", description: "Adjunta documentos que permitan comprobar el precio declarado de los productos.", on_changed: move |_| on_case_changed.call(()) }
            } else if step == 4 {
                CaptureEvidenceStep { case_id, kind: EvidenceKind::BankStatement, title: "Estados de cuenta y cargos", description: "Agrega evidencia de los cargos realizados para respaldar el pago de la compra.", on_changed: move |_| on_case_changed.call(()) }
            } else {
                div { class: "capture-review",
                    span { class: "eyebrow", "PASO 6 DE 6" }
                    h2 { "Revisa tu rectificación" }
                    p { "Verifica que la información obligatoria esté completa antes de abrir el expediente." }
                    div { class: "capture-review-list",
                        article { class: "complete", strong { "Información general" } span { "{applicant_profile.full_name}" } small { "✓ Completo" } }
                        article { class: "complete", strong { "Envío y guía" } span { "{case.tracking_number}" } small { "✓ Completo" } }
                        article { class: if products.is_empty() { "warning" } else { "complete" }, strong { "Productos" } span { "{product_count} productos" } small { if products.is_empty() { "⚠ Agrega al menos uno" } else { "✓ Completo" } } }
                        article { class: if value_count == 0 { "warning" } else { "complete" }, strong { "Comprobantes" } span { "{value_count} archivos" } small { if value_count == 0 { "⚠ Falta documentación del valor" } else { "✓ Completo" } } }
                        article { strong { "Estados de cuenta" } span { "{payment_count} archivos" } small { if payment_count == 0 { "Recomendado" } else { "✓ Completo" } } }
                    }
                }
            }

            div { class: "capture-navigation",
                button { class: "text-button", disabled: step <= 2, onclick: move |_| on_step_changed.call(step.saturating_sub(1)), "← Atrás" }
                span { "✓ Guardado localmente" }
                if step < 5 {
                    button { class: "primary-button", disabled: (step == 2 && products.is_empty()) || (step == 3 && value_count == 0), onclick: move |_| on_step_changed.call(step + 1), "Guardar y continuar" }
                } else {
                    button { class: "primary-button", disabled: !can_finish, onclick: move |_| on_finished.call(()), "Abrir expediente" }
                }
            }
        }
    }
}

#[component]
fn CaseDetail(
    case: RectificationCase,
    applicant_profile: ApplicantProfile,
    on_case_changed: EventHandler<()>,
    on_archive_changed: EventHandler<bool>,
    on_deleted: EventHandler<()>,
) -> Element {
    let case_id = case.id;
    let case_is_archived = case.archived_at.is_some();
    let mut panel = use_signal(move || EvidencePanelState::load(case_id));
    let mut evidence_kind = use_signal(|| EvidenceKind::Transaction.as_str().to_owned());
    let mut evidence_title = use_signal(String::new);
    let mut preview = use_signal(|| None::<EvidencePreview>);
    let mut pending_removal = use_signal(|| None::<Uuid>);
    let mut pending_case_deletion = use_signal(|| false);
    let mut notice = use_signal(|| None::<String>);

    let panel_snapshot = panel.read().clone();
    let case_products = CaseDataStore::open_default()
        .and_then(|store| store.list_products(case_id))
        .unwrap_or_default();
    let product_total_mxn = case_products
        .iter()
        .fold(Decimal::ZERO, |total, product| total + product.total_mxn);
    let case_product_count = case_products.len();
    let value_document_count = panel_snapshot
        .documents
        .iter()
        .filter(|document| document.kind == EvidenceKind::Product)
        .count();
    let payment_document_count = panel_snapshot
        .documents
        .iter()
        .filter(|document| document.kind == EvidenceKind::BankStatement)
        .count();
    let case_updated_at = case
        .updated_at
        .with_timezone(&Local)
        .format("%d %b %Y · %H:%M")
        .to_string();
    let pending_document = pending_removal.read().and_then(|id| {
        panel_snapshot
            .documents
            .iter()
            .find(|item| item.id == id)
            .cloned()
    });

    rsx! {
        section { class: "case-detail",
            div { class: "case-heading",
                div {
                    span { class: "eyebrow", "EXPEDIENTE" }
                    h3 { "{case.display_name}" }
                    p { "Guía {case.tracking_number} · Última actualización {case_updated_at}" }
                }
                div { class: "case-heading-actions",
                    button {
                        class: if case_is_archived { "secondary-button restore-case-button" } else { "secondary-button archive-case-button" },
                        onclick: move |_| {
                            let should_archive = !case_is_archived;
                            let result = CaseDataStore::open_default()
                                .map_err(|error| error.to_string())
                                .and_then(|store| {
                                    store
                                        .set_case_archived(case_id, should_archive)
                                        .map_err(|error| error.to_string())
                                });
                            match result {
                                Ok(()) => {
                                    notice.set(Some(if should_archive {
                                        "La rectificación se archivó correctamente".to_owned()
                                    } else {
                                        "La rectificación volvió a la lista activa".to_owned()
                                    }));
                                    on_archive_changed.call(should_archive);
                                }
                                Err(error) => panel.write().error = Some(error),
                            }
                        },
                        if case_is_archived { "Restaurar" } else { "Archivar" }
                    }
                    button {
                        class: "danger-button delete-case-button",
                        onclick: move |_| pending_case_deletion.set(true),
                        "Eliminar"
                    }
                    button {
                        class: "secondary-button",
                        onclick: move |_| {
                            notice.set(None);
                            let Some(destination) = rfd::FileDialog::new().pick_folder() else {
                                return;
                            };
                            let result = panel
                                .read()
                                .vault
                                .as_ref()
                                .ok_or_else(|| "El almacén cifrado no está disponible".to_owned())
                                .and_then(|vault| vault.export_case(case_id, &destination).map_err(|error| error.to_string()));
                            match result {
                                Ok(path) => {
                                    notice.set(Some(format!("Expediente exportado en {}", path.display())));
                                    panel.write().reload(case_id);
                                }
                                Err(error) => panel.write().error = Some(error),
                            }
                        },
                        "Respaldar archivos"
                    }
                    span { class: "status-pill", "{case.status.label()}" }
                }
            }

            if *pending_case_deletion.read() {
                div { class: "removal-confirmation case-delete-confirmation",
                    div {
                        strong { "¿Eliminar definitivamente {case.display_name}?" }
                        p { "Se borrarán sus productos, rastreo, evidencias cifradas y documentos preparados. Esta acción no se puede deshacer." }
                    }
                    div {
                        button { class: "text-button", onclick: move |_| pending_case_deletion.set(false), "Cancelar" }
                        button {
                            class: "danger-button",
                            onclick: move |_| {
                                let result = EvidenceVault::open_default()
                                    .and_then(|vault| vault.delete_case(case_id))
                                    .map_err(|error| error.to_string());
                                match result {
                                    Ok(()) => {
                                        pending_case_deletion.set(false);
                                        on_deleted.call(());
                                    }
                                    Err(error) => panel.write().error = Some(error),
                                }
                            },
                            "Sí, eliminar expediente"
                        }
                    }
                }
            }

            section { class: "economic-summary",
                div { span { "Valor total de productos" } strong { "${format_money(product_total_mxn)} MXN" } }
                div { span { "Productos" } strong { "{case_product_count}" } }
                div { span { "Documentos de valor" } strong { "{value_document_count}" } }
                div { span { "Comprobantes de pago" } strong { "{payment_document_count}" } }
            }

            ProductPanel {
                case_id,
                on_changed: move |_| panel.write().reload(case_id),
            }

            TrackingPanel {
                case: case.clone(),
                on_changed: move |_| {
                    panel.write().reload(case_id);
                    on_case_changed.call(());
                },
            }

            DocumentPanel {
                key: "documents-{case.id}",
                case: case.clone(),
                applicant_profile: applicant_profile.clone(),
                on_changed: move |_| {
                    panel.write().reload(case_id);
                    on_case_changed.call(());
                },
            }

            div { hidden: !SHOW_EMAIL_WORKFLOW,
                EmailPanel {
                    key: "{case.updated_at}",
                    case: case.clone(),
                    applicant_profile,
                    on_changed: move |_| {
                        panel.write().reload(case_id);
                        on_case_changed.call(());
                    },
                }
            }

            section { class: "evidence-section",
                div { class: "section-heading",
                    div {
                        span { class: "eyebrow", "BÓVEDA LOCAL" }
                        h4 { "Evidencias del expediente" }
                        p { "PDF e imágenes de hasta 25 MB. Se cifran antes de guardarse." }
                    }
                    span { class: "evidence-count", "{panel_snapshot.documents.len()} archivos" }
                }

                if let Some(error) = panel_snapshot.error.as_ref() {
                    div { class: "alert error", "{error}" }
                }
                if let Some(message) = notice.read().as_ref() {
                    div { class: "alert success", "{message}" }
                }

                div { class: "evidence-toolbar",
                    label {
                        span { "Tipo" }
                        select {
                            value: "{evidence_kind}",
                            onchange: move |event| evidence_kind.set(event.value()),
                            option { value: "customs_form", "Boleta aduanal" }
                            option { value: "transaction", "Comprobante de transacción" }
                            option { value: "bank_statement", "Estado de cuenta" }
                            option { value: "product", "Factura o producto" }
                            option { value: "other", "Otro anexo" }
                        }
                    }
                    label { class: "evidence-title-field",
                        span { "Título" }
                        input {
                            value: "{evidence_title}",
                            placeholder: "Opcional",
                            oninput: move |event| evidence_title.set(event.value()),
                        }
                    }
                    button {
                        class: "primary-button attach-button",
                        onclick: move |_| {
                            notice.set(None);
                            let Some(path) = rfd::FileDialog::new()
                                .add_filter("PDF e imágenes", &["pdf", "png", "jpg", "jpeg", "webp"])
                                .pick_file()
                            else {
                                return;
                            };
                            let kind = match EvidenceKind::from_str(&evidence_kind.read()) {
                                Ok(kind) => kind,
                                Err(error) => {
                                    panel.write().error = Some(error.to_string());
                                    return;
                                }
                            };
                            let title = Some(evidence_title.read().clone());
                            let result = panel
                                .read()
                                .vault
                                .as_ref()
                                .ok_or_else(|| "El almacén cifrado no está disponible".to_owned())
                                .and_then(|vault| vault.import_evidence(case_id, kind, title, &path).map_err(|error| error.to_string()));
                            match result {
                                Ok(document) => {
                                    evidence_title.set(String::new());
                                    notice.set(Some(format!("{} se cifró y agregó al expediente", document.original_filename)));
                                    panel.write().reload(case_id);
                                }
                                Err(error) => panel.write().error = Some(error),
                            }
                        },
                        "+ Adjuntar archivo"
                    }
                }

                if let Some(document) = pending_document {
                    div { class: "removal-confirmation",
                        div {
                            strong { "¿Retirar {document.title}?" }
                            p { "Se eliminará la copia cifrada local. El evento permanecerá en la bitácora." }
                        }
                        div {
                            button { class: "text-button", onclick: move |_| pending_removal.set(None), "Cancelar" }
                            button {
                                class: "danger-button",
                                onclick: move |_| {
                                    let result = panel
                                        .read()
                                        .vault
                                        .as_ref()
                                        .ok_or_else(|| "El almacén cifrado no está disponible".to_owned())
                                        .and_then(|vault| vault.remove_evidence(&document).map_err(|error| error.to_string()));
                                    match result {
                                        Ok(()) => {
                                            pending_removal.set(None);
                                            preview.set(None);
                                            notice.set(Some("Evidencia retirada correctamente".to_owned()));
                                            panel.write().reload(case_id);
                                        }
                                        Err(error) => panel.write().error = Some(error),
                                    }
                                },
                                "Retirar definitivamente"
                            }
                        }
                    }
                }

                if panel_snapshot.documents.is_empty() {
                    div { class: "evidence-empty",
                        span { "▣" }
                        strong { "Todavía no hay evidencias" }
                        p { "Adjunta la boleta, transacción, estado de cuenta o captura del producto." }
                    }
                } else {
                    div { class: "evidence-layout",
                        div { class: "evidence-list",
                            for (index, document) in panel_snapshot.documents.iter().enumerate() {
                                {
                                    let preview_document = document.clone();
                                    let remove_id = document.id;
                                    let up_id = document.id;
                                    let down_id = document.id;
                                    rsx! {
                                        article {
                                            key: "{document.id}",
                                            class: "evidence-row",
                                            title: "Abrir evidencia en el visor",
                                            onclick: move |_| {
                                                let result = panel.read().vault.as_ref()
                                                    .ok_or_else(|| "Bóveda no disponible".to_owned())
                                                    .and_then(|vault| vault.load_evidence_bytes(&preview_document).map_err(|error| error.to_string()));
                                                match result {
                                                    Ok(bytes) => {
                                                        let data_url = format!(
                                                            "data:{};base64,{}",
                                                            preview_document.content_type,
                                                            STANDARD.encode(bytes)
                                                        );
                                                        preview.set(Some(EvidencePreview {
                                                            document: preview_document.clone(),
                                                            data_url,
                                                        }));
                                                    }
                                                    Err(error) => panel.write().error = Some(error),
                                                }
                                            },
                                            div { class: "file-kind-icon", if document.is_image() { "IMG" } else { "PDF" } }
                                            div { class: "evidence-meta",
                                                strong { "{document.title}" }
                                                span { "{document.kind.label()} · {document.size_label()}" }
                                                small { "SHA-256 {&document.sha256[..12]}…" }
                                            }
                                            div { class: "evidence-actions", onclick: move |event| event.stop_propagation(),
                                                button {
                                                    title: "Subir",
                                                    disabled: index == 0,
                                                    onclick: move |_| {
                                                        let result = panel.read().vault.as_ref()
                                                            .ok_or_else(|| "Bóveda no disponible".to_owned())
                                                            .and_then(|vault| vault.move_evidence(case_id, up_id, -1).map_err(|error| error.to_string()));
                                                        match result {
                                                            Ok(()) => panel.write().reload(case_id),
                                                            Err(error) => panel.write().error = Some(error),
                                                        }
                                                    },
                                                    "↑"
                                                }
                                                button {
                                                    title: "Bajar",
                                                    disabled: index + 1 == panel_snapshot.documents.len(),
                                                    onclick: move |_| {
                                                        let result = panel.read().vault.as_ref()
                                                            .ok_or_else(|| "Bóveda no disponible".to_owned())
                                                            .and_then(|vault| vault.move_evidence(case_id, down_id, 1).map_err(|error| error.to_string()));
                                                        match result {
                                                            Ok(()) => panel.write().reload(case_id),
                                                            Err(error) => panel.write().error = Some(error),
                                                        }
                                                    },
                                                    "↓"
                                                }
                                                button { class: "remove-icon-button", onclick: move |_| pending_removal.set(Some(remove_id)), "×" }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        aside { class: "preview-panel",
                            if let Some(current) = preview.read().as_ref() {
                                div { class: "preview-heading",
                                    strong { "{current.document.title}" }
                                    span { "Descifrado sólo en memoria" }
                                }
                                if current.document.is_image() {
                                    img { src: "{current.data_url}", alt: "Vista previa de {current.document.title}" }
                                } else {
                                    iframe { class: "pdf-document-preview", src: "{current.data_url}", title: "Vista previa de {current.document.title}" }
                                }
                            } else {
                                div { class: "preview-placeholder",
                                    span { "◫" }
                                    p { "Haz clic sobre una evidencia para visualizarla sin crear archivos temporales." }
                                }
                            }
                        }
                    }
                }
            }

            section { class: "audit-section",
                div { class: "section-heading small",
                    div {
                        span { class: "eyebrow", "TRAZABILIDAD" }
                        h4 { "Actividad reciente" }
                    }
                }
                div { class: "audit-list",
                    for event in panel_snapshot.audit_events.iter().take(6) {
                        {
                            let timestamp = event
                                .created_at
                                .with_timezone(&Local)
                                .format("%d/%m/%Y · %H:%M")
                                .to_string();
                            rsx! {
                                div { key: "{event.id}", class: "audit-row",
                                    span { class: "audit-dot" }
                                    div {
                                        strong { "{event.summary}" }
                                        small { "{timestamp}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn StepCard(
    number: &'static str,
    title: &'static str,
    description: &'static str,
    active: bool,
) -> Element {
    rsx! {
        article { class: if active { "step-card active" } else { "step-card" },
            span { class: "step-number", "{number}" }
            div {
                strong { "{title}" }
                p { "{description}" }
            }
            span { class: "step-state", if active { "✓" } else { "·" } }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_initial_render_builds_without_panicking() {
        let mut virtual_dom = VirtualDom::new(App);
        virtual_dom.rebuild_in_place();
    }

    #[test]
    fn exchange_rate_client_runs_and_drops_on_a_blocking_thread() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 8, 14).unwrap();
        let rate = runtime.block_on(async move {
            tokio::task::spawn_blocking(move || fetch_exchange_rate("MXN", date))
                .await
                .unwrap()
                .unwrap()
        });
        assert_eq!(rate.currency, "MXN");
        assert_eq!(rate.rate_to_mxn, Decimal::ONE);
    }

    #[test]
    fn automatic_tracking_refresh_waits_twelve_hours_between_attempts() {
        let now = DateTime::parse_from_rfc3339("2026-08-15T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        assert!(automatic_tracking_refresh_due(None, now));
        assert!(!automatic_tracking_refresh_due(
            Some(now - chrono::Duration::hours(11) - chrono::Duration::minutes(59)),
            now,
        ));
        assert!(automatic_tracking_refresh_due(
            Some(now - chrono::Duration::hours(12)),
            now,
        ));
    }

    #[test]
    fn new_case_wizard_validates_the_tracking_number_before_advancing() {
        assert!(validate_wizard_tracking_number("ZZ000000000ZZ").is_ok());
        assert!(validate_wizard_tracking_number("GUIA-CORTA").is_err());
    }

    #[test]
    fn applicant_profile_validation_is_shared_by_onboarding_and_settings() {
        let mut profile = ApplicantProfile::default();
        assert_eq!(
            validate_applicant_profile(&profile),
            Err("Escribe el nombre completo del solicitante")
        );

        profile.full_name = "María Peña".to_owned();
        profile.email = "correo-invalido".to_owned();
        assert_eq!(
            validate_applicant_profile(&profile),
            Err("El correo electrónico no tiene un formato válido")
        );

        profile.email = "maria@example.test".to_owned();
        profile.postal_code = "8500A".to_owned();
        assert_eq!(
            validate_applicant_profile(&profile),
            Err("El código postal debe contener exactamente cinco dígitos")
        );

        profile.postal_code = "85000".to_owned();
        assert_eq!(validate_applicant_profile(&profile), Ok(()));
    }
}
