-- Descarta únicamente eventos derivados por la versión defectuosa del parser,
-- que guardaba la secuencia oculta del portal como descripción. Las respuestas
-- crudas permanecen intactas y la siguiente consulta reconstruye los eventos.
DELETE FROM tracking_events
WHERE source = 'correos_mexico'
  AND description <> ''
  AND description NOT GLOB '*[^0-9]*';

UPDATE rectification_cases
SET has_unseen_updates = CASE
    WHEN EXISTS (
        SELECT 1
        FROM tracking_events
        WHERE tracking_events.case_id = rectification_cases.id
          AND tracking_events.is_seen = 0
    ) THEN 1
    ELSE 0
END;
