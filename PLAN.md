# Plan de producto y desarrollo — Mi Rectificación MX

## 1. Objetivo

Crear una aplicación local-first, multiplataforma y escrita en Rust con Dioxus para preparar expedientes de rectificación de boletas aduanales mal valuadas en México.

La aplicación deberá:

- guiar la creación de una rectificación desde una pantalla principal;
- conservar y mostrar las rectificaciones existentes en una barra lateral;
- señalar expedientes con cambios de rastreo o tareas pendientes;
- generar un PDF de solicitud de rectificación;
- generar un segundo PDF con pruebas y un índice de anexos;
- registrar nombre, cantidad, precio y moneda original de cada producto;
- convertir importes a MXN y dejar evidencia auditable de la tasa utilizada;
- consultar periódicamente el rastreo de Correos de México;
- redactar un correo con asunto, cuerpo y adjuntos listo para revisión;
- exportar el expediente completo sin enviar datos automáticamente a terceros.

La captura adjunta se toma únicamente como referencia visual: navegación lateral oscura, lista persistente, indicador de actualización y área principal enfocada. No contiene requisitos ni instrucciones adicionales.

## 2. Alcance de la primera versión

### Incluido en el MVP

1. Aplicación de escritorio para macOS, Windows y Linux desde una sola base de código.
2. Expedientes y adjuntos almacenados localmente.
3. Asistente para crear y completar una rectificación.
4. Carga de imágenes y PDF para:
   - boleta aduanal;
   - comprobante de transacción;
   - estado de cuenta;
   - factura, recibo o captura del producto;
   - otros anexos.
5. Productos múltiples, cantidades, precio, envío informativo, impuestos y moneda ISO 4217.
6. Conversión a MXN con tasa, fecha, fuente y resultado guardados en el expediente.
7. Generación reproducible de dos PDF:
   - `Solicitud_de_rectificacion.pdf`;
   - `Pruebas_y_anexos.pdf`.
8. Consulta de rastreo al abrir la app y en intervalos configurables mientras esté activa.
9. Línea de tiempo de movimientos y punto de color para novedades no revisadas.
10. Generación de borrador de correo y archivo `.eml`, con los PDF adjuntos cuando el cliente de correo lo permita.
11. Exportación de un paquete ZIP con PDF, originales y manifiesto de integridad.

### Fuera del MVP

- envío desatendido de correos;
- presentación automática ante una autoridad;
- decisión legal sobre si el trámite procede;
- OCR como fuente definitiva de importes;
- sincronización en la nube o trabajo multiusuario;
- aplicación móvil completa;
- ejecución permanente en segundo plano cuando la app está cerrada.

Estas capacidades se podrán agregar después de validar el procedimiento real, el destinatario y las reglas de cada aduana.

## 3. Principios del producto

- **Local-first:** estados de cuenta y comprobantes no salen del equipo salvo una exportación o envío iniciado por la persona.
- **Auditable:** no se sobrescriben silenciosamente valores, tasas ni movimientos; cada cambio conserva procedencia y fecha.
- **Revisión humana:** la app redacta y prepara, pero la persona confirma datos, destinatarios, documentos y envío.
- **Tolerante a fallas:** el rastreo y el tipo de cambio son adaptadores reemplazables; una caída externa no bloquea el expediente.
- **Sin asesoría legal implícita:** las plantillas son ayuda de redacción y deberán validarse con un caso real antes de producción.

## 4. Experiencia de usuario

### Estructura tipo Codex

**Barra lateral**

- botón `Nueva rectificación`;
- buscador y filtros;
- expedientes recientes con nombre corto, guía, estado y última actualización;
- punto azul para novedades de rastreo no vistas;
- punto ámbar para información o documentos faltantes;
- agrupación opcional: borradores, en preparación, enviados y cerrados;
- acceso a ajustes y perfil del solicitante.

**Pantalla principal sin selección**

- título `Iniciar una nueva rectificación`;
- formulario resumido con número de guía, número/folio de boleta, producto, valor aduanal observado y carga de documentos;
- botón `Crear expediente y continuar`.

**Detalle de expediente**

- encabezado con estado y acciones principales;
- pasos: Datos, Productos, Valoración, Evidencias, Rastreo, Documentos y Correo;
- panel de tareas pendientes y validaciones;
- vista previa de ambos PDF;
- línea de tiempo única para cambios locales, rastreo, exportaciones y correos preparados.

### Estados sugeridos

`Borrador → Faltan pruebas → Listo para generar → Documentos generados → Correo preparado → Enviado → Resuelto/Cerrado`

El estado de negocio y el indicador de novedades deben ser campos separados. Abrir una novedad la marca como vista, sin cambiar el estado del trámite.

## 5. Datos del expediente

### Solicitante

- nombre completo;
- correo y teléfono opcionales;
- domicilio sólo cuando la plantilla confirmada lo requiera;
- datos fiscales o identificadores sólo cuando sean indispensables.

### Envío y boleta

- número de guía de 13 caracteres;
- año de consulta;
- transportista/origen;
- folio y fecha de la boleta;
- aduana u oficina identificada;
- valor determinado por la autoridad;
- contribuciones cobradas, si aparecen;
- observaciones.

### Productos y cálculo

- nombre y descripción;
- cantidad;
- precio unitario y total;
- envío pagado como dato informativo, excluido de la valoración;
- impuestos pagados;
- moneda original;
- fecha relevante para el tipo de cambio;
- tasa a MXN, proveedor, hora de consulta y método de redondeo;
- total convertido a MXN;
- valor aduanal observado y diferencia.

Usar decimales exactos, nunca `float`, y guardar la instantánea de la tasa. Una actualización posterior no debe alterar un PDF ya generado.

### Evidencias

Cada archivo tendrá tipo, título, fecha, descripción, origen, orden, hash SHA-256 y una copia inmutable usada en cada generación. La app debe permitir ocultar o recortar datos no necesarios antes de incorporarlos al PDF, conservando el original local.

## 6. Arquitectura propuesta

### Base tecnológica

- Rust estable;
- Dioxus 0.7 para interfaz compartida y empaquetado de escritorio;
- HTML/CSS propio para reproducir el lenguaje visual oscuro sin copiar marcas ni recursos de Codex;
- `tokio` para tareas asíncronas;
- SQLite con migraciones para datos estructurados;
- directorio privado de la aplicación para adjuntos;
- llavero del sistema para la clave local;
- cifrado autenticado de adjuntos sensibles;
- motor de plantillas PDF ejecutado en proceso, con tipografías embebidas;
- `reqwest` y parsers específicos detrás de traits para servicios externos.

Dioxus declara soporte desde una sola base de código para web, escritorio y móvil, además de empaquetado para macOS, Linux y Windows. El MVP debe apuntar a escritorio; compartir código no elimina el trabajo de empaquetado, permisos y pruebas por plataforma.

### Workspace sugerido

```text
mi-rectificacion-mx/
├── apps/desktop/                 # shell Dioxus y adaptadores del SO
├── crates/domain/                # entidades, estados y reglas
├── crates/application/           # casos de uso
├── crates/storage/               # SQLite, migraciones y archivos
├── crates/documents/             # plantillas y generación PDF/EML/ZIP
├── crates/integrations/          # rastreo y tipos de cambio
├── crates/security/              # cifrado, hashes y redacción
├── assets/                       # CSS, iconos y fuentes con licencia
├── templates/                    # solicitud, anexos y correo
└── tests/fixtures/               # casos anonimizados
```

### Entidades principales

- `RectificationCase`
- `ApplicantProfile`
- `Shipment`
- `CustomsAssessment`
- `ProductLine`
- `ExchangeRateSnapshot`
- `EvidenceDocument`
- `TrackingEvent`
- `GeneratedArtifact`
- `EmailDraft`
- `AuditEvent`
- `Notification`

## 7. Integraciones

### Tipo de cambio

Definir `ExchangeRateProvider` y comenzar con un proveedor oficial o institucional que cubra la moneda requerida. Antes de cerrar la implementación se debe validar qué fecha y qué fuente corresponden al procedimiento aduanal real.

Flujo:

1. seleccionar moneda y fecha;
2. consultar la tasa;
3. mostrar fuente, par, fecha y cálculo;
4. permitir corrección manual con motivo obligatorio;
5. congelar la instantánea al generar;
6. incluir una nota de cálculo en el PDF de pruebas.

Si el proveedor no ofrece la moneda o está caído, la app debe permitir capturar una tasa documentada y adjuntar su comprobante. Nunca debe inventar ni reutilizar silenciosamente una tasa antigua.

### Correos de México

El portal oficial observado utiliza un formulario ASP.NET con campos de estado y una petición POST, no una API pública documentada visible. Por ello:

- implementar `TrackingProvider` separado del dominio;
- mantener cookies y campos de estado por consulta;
- limitar frecuencia, aplicar timeout, reintentos con backoff y caché;
- normalizar movimientos sin borrar la respuesta cruda;
- detectar nuevos eventos por una clave estable;
- consultar al iniciar y cada 4–6 horas mientras la app esté abierta;
- ofrecer `Abrir rastreo oficial` y captura manual cuando cambie el portal;
- no intentar evadir CAPTCHA, bloqueos ni límites del sitio;
- revisar términos de uso antes de distribuir esta automatización.

Una tarea del sistema operativo cuando la app esté cerrada queda para una fase posterior y requerirá implementación y permisos distintos por plataforma.

### Correo

La app generará:

- destinatario configurable y validado para el caso;
- asunto con guía y folio;
- cuerpo editable a partir de una plantilla;
- lista de anexos;
- archivo `.eml` y acción para abrir el cliente predeterminado.

No se codificará una dirección de Aduanas sin confirmarla con documentación vigente o una boleta real. El envío directo con OAuth/SMTP sólo se considerará después, con vista previa, confirmación inmediata, registro de consentimiento y sin guardar contraseñas.

## 8. Contenido de los PDF

### PDF 1 — Solicitud de rectificación

1. lugar y fecha;
2. destinatario confirmado;
3. identificación del envío y la boleta;
4. relato breve y factual;
5. tabla `valor indicado / valor acreditado / diferencia`;
6. petición concreta, sin afirmar facultades o fundamentos no validados;
7. lista numerada de anexos;
8. datos de contacto y espacio de firma;
9. pie con identificador del expediente y versión del documento.

### PDF 2 — Pruebas y anexos

1. portada e índice;
2. resumen de productos e importes originales;
3. tabla de conversión a MXN con fórmula, tasa, fecha y fuente;
4. comprobantes de compra/transacción;
5. estado de cuenta;
6. factura o página/captura del producto;
7. boleta aduanal;
8. historial de rastreo;
9. otros anexos;
10. manifiesto de archivos y hashes.

Cada imagen debe respetar orientación, márgenes, legibilidad y paginación. El sistema no debe reescalar capturas hasta volver ilegibles importes o identificadores.

## 9. Fases de implementación

### Fase 0 — Descubrimiento y caso patrón

- conseguir una boleta anonimizada y un expediente real resuelto o aceptado;
- confirmar autoridad, dirección de correo, asunto requerido, plazo, documentos y regla de conversión;
- revisar términos del portal de rastreo;
- definir qué datos deben ocultarse;
- aprobar los dos esquemas de PDF y la plantilla de correo.

**Salida:** especificación validada y fixtures anonimizados. Sin esto no se debe prometer automatización legal completa.

### Fase 1 — Fundación y diseño

- crear repositorio y workspace Rust;
- fijar versión de Dioxus y toolchain;
- configurar formato, lint, pruebas y CI para tres escritorios;
- construir shell visual, barra lateral, tema oscuro y navegación;
- definir esquema SQLite y migraciones.

**Criterio:** crear, cerrar y reabrir un expediente conservando sus datos.

### Fase 2 — Captura y seguridad local

- implementar asistente y validaciones;
- adjuntar, ordenar, previsualizar y retirar documentos;
- cifrar adjuntos y proteger la clave con el sistema;
- agregar hashes y bitácora;
- implementar respaldo/exportación local.

**Criterio:** ningún archivo sensible queda en temporales sin limpieza y el expediente sobrevive reinicios.

### Fase 3 — Productos y conversión

**Estado:** completada el 14 de agosto de 2026 con almacenamiento decimal exacto, consulta Frankfurter, fallback manual justificado, tabla comparativa y pruebas JPY/USD.

- modelar productos múltiples y totales;
- integrar proveedor de tasas;
- guardar instantáneas y overrides justificados;
- crear tabla comparativa y pruebas unitarias de redondeo.

**Criterio:** los cálculos son reproducibles con fixtures de varias monedas y cantidades; descuentos y envío no alteran la valoración.

### Fase 4 — Documentos

**Estado:** completada el 15 de agosto de 2026 con plantillas versionadas, solicitud PDF, dossier multipágina, correo `.eml`, manifiesto y ZIP revisable.

- implementar plantillas versionadas;
- generar solicitud y dossier de pruebas;
- previsualizar y regenerar;
- producir `.eml`, ZIP y manifiesto;
- probar capturas verticales, horizontales y PDF multipágina.

**Criterio:** ambos PDF pasan comparación visual, extracción de texto y revisión manual con un caso patrón.

### Fase 5 — Rastreo y notificaciones

**Estado:** completada el 15 de agosto de 2026 con adaptador de Correos de México, respuestas crudas, eventos normalizados y deduplicados, actualización al inicio y cada 15 minutos, indicador de novedades y fallback manual.

- implementar adaptador de Correos de México;
- persistir respuesta cruda y eventos normalizados;
- programar actualización al inicio y periódica;
- mostrar novedades no vistas y errores recuperables;
- agregar apertura/manual fallback.

**Criterio:** un nuevo movimiento crea exactamente una notificación; una respuesta repetida no duplica eventos.

### Fase 6 — Correo y cierre del flujo

**Estado:** completada el 15 de agosto de 2026 con borrador persistente y editable, archivo `.eml` multipart con ambos PDF adjuntos, apertura explícita en el cliente de correo, bitácora y marcado manual como enviado.

- generar destinatario, asunto, cuerpo y anexos desde datos confirmados;
- abrir el borrador en el cliente de correo o guardar `.eml`;
- registrar `correo preparado` y permitir marcar `enviado` manualmente;
- completar accesibilidad, atajos, estados vacíos y errores.

**Criterio:** la persona puede revisar cada dato y adjunto antes de salir de la app; no ocurre transmisión sin acción explícita.

### Fase 7 — Empaquetado y piloto

- firmar/notarizar donde corresponda;
- generar instaladores para macOS, Windows y Linux;
- probar rutas, permisos, llavero, fuentes y cliente de correo en cada SO;
- ejecutar piloto con expedientes anonimizados y luego con un caso real bajo supervisión;
- documentar respaldo, actualización y recuperación.

**Criterio:** matriz de pruebas aprobada en los tres sistemas y cero pérdida de adjuntos ante actualización.

## 10. Estrategia de pruebas

- unitarias para importes, estados, deduplicación y plantillas;
- integración con SQLite y migraciones;
- fixtures HTTP grabados y anonimizados para rastreo y tipos de cambio;
- contract tests que alerten cuando cambie el HTML de Correos de México;
- golden tests para estructura y texto de PDF;
- render de todas las páginas a imagen y comparación visual con tolerancia;
- pruebas de cifrado, claves inválidas, corrupción y restauración;
- pruebas end-to-end del flujo completo;
- matriz manual macOS/Windows/Linux para selectores, cliente de correo y empaquetado.

## 11. Riesgos y mitigaciones

| Riesgo | Mitigación |
|---|---|
| Cambio del portal de rastreo | Adaptador aislado, contract tests, caché y captura manual |
| Fuente o fecha de tipo de cambio incorrecta | Validación en Fase 0, instantánea auditable y override justificado |
| Dirección o procedimiento aduanal variable | Destinatario configurable y plantilla validada por caso |
| Exposición de estados de cuenta | Local-first, cifrado, redacción y confirmación de exportación |
| PDF ilegible o incompleto | Vista previa, render visual, índice y validaciones de resolución |
| Diferencias entre plataformas | Adaptadores de SO y CI/QA en cada objetivo |
| Automatización interpretada como asesoría legal | Texto neutral, campos revisables y aviso claro de alcance |
| Pérdida de información | Guardado transaccional, copias inmutables, exportación y pruebas de recuperación |

## 12. Orden recomendado de entrega

El primer incremento útil debe terminar en **captura local + conversión auditable + dos PDF correctos**. Después se agrega rastreo, luego preparación de correo y finalmente empaquetado multiplataforma. Así se valida primero el valor central sin depender del portal externo ni de un destinatario todavía no confirmado.

## 13. Decisiones que deben validarse antes de programar la integración final

1. ¿Qué documento se llama exactamente “boleta” en los casos objetivo y qué campos trae?
2. ¿Qué autoridad, oficina y correo recibe la solicitud en cada caso?
3. ¿Qué plazo y referencia legal aplican?
4. ¿Qué fecha y fuente de tipo de cambio acepta la autoridad?
5. Decisión confirmada: el envío pagado se conserva como referencia, pero no forma parte del valor acreditado.
6. ¿Se requiere firma autógrafa, electrónica o sólo identificación?
7. ¿Se envían dos PDF separados, un PDF unido o un paquete de originales?
8. ¿Qué datos del estado de cuenta pueden o deben ocultarse?
9. ¿La consulta automatizada del portal está permitida para una app distribuida?

## 14. Referencias técnicas verificadas para este plan

- Dioxus: <https://github.com/DioxusLabs/dioxus>
- Documentación Dioxus 0.7: <https://dioxuslabs.com/learn/0.7/>
- Rastreo oficial de Correos de México: <https://www.correosdemexico.gob.mx/SSLServicios/SeguimientoEnvio/Seguimiento.aspx>
