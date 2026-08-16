use mi_rectificacion_domain::{DomainError, RectificationCase};
use thiserror::Error;
use uuid::Uuid;

pub trait CaseRepository {
    type Error: std::fmt::Debug + std::fmt::Display + Send + Sync + 'static;

    fn list(&self) -> Result<Vec<RectificationCase>, Self::Error>;
    fn insert(&self, case: &RectificationCase) -> Result<(), Self::Error>;
}

#[derive(Debug, Clone, Default)]
pub struct CreateCaseInput {
    pub display_name: Option<String>,
    pub tracking_number: String,
    pub customs_form_number: Option<String>,
}

pub fn create_case<R: CaseRepository>(
    repository: &R,
    input: CreateCaseInput,
) -> Result<RectificationCase, ApplicationError<R::Error>> {
    let case = RectificationCase::new(
        input.tracking_number,
        input.customs_form_number,
        input.display_name,
    )?;
    if let Some(existing) = repository
        .list()
        .map_err(ApplicationError::Storage)?
        .into_iter()
        .find(|existing| existing.tracking_number == case.tracking_number)
    {
        return Err(ApplicationError::DuplicateTracking {
            case_id: existing.id,
            tracking_number: existing.tracking_number,
        });
    }
    repository
        .insert(&case)
        .map_err(ApplicationError::Storage)?;
    Ok(case)
}

#[derive(Debug, Error)]
pub enum ApplicationError<E: std::fmt::Debug + std::fmt::Display + 'static> {
    #[error(transparent)]
    Domain(#[from] DomainError),
    #[error("La guía {tracking_number} ya tiene una rectificación guardada")]
    DuplicateTracking {
        case_id: Uuid,
        tracking_number: String,
    },
    #[error("No fue posible guardar el expediente: {0}")]
    Storage(E),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct MemoryRepository {
        cases: RefCell<Vec<RectificationCase>>,
    }

    impl CaseRepository for MemoryRepository {
        type Error = String;

        fn list(&self) -> Result<Vec<RectificationCase>, Self::Error> {
            Ok(self.cases.borrow().clone())
        }

        fn insert(&self, case: &RectificationCase) -> Result<(), Self::Error> {
            self.cases.borrow_mut().push(case.clone());
            Ok(())
        }
    }

    #[test]
    fn returns_the_existing_case_for_a_duplicate_tracking_number() {
        let repository = MemoryRepository::default();
        let existing = RectificationCase::new("ZZ000000000ZZ", None, None).unwrap();
        repository.cases.borrow_mut().push(existing.clone());

        let error = create_case(
            &repository,
            CreateCaseInput {
                tracking_number: "zz000000000zz".to_owned(),
                ..CreateCaseInput::default()
            },
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ApplicationError::DuplicateTracking { case_id, .. } if case_id == existing.id
        ));
        assert_eq!(repository.cases.borrow().len(), 1);
    }
}
