//! Validator files: the `.validators.slcr.<ext>` files beside the code
//! that bind requirements to the tools that check them.
//!
//! The format is the validator file JSON Schema on the Notion "Schemas"
//! page (`https://slcr.io/reference/schemas/validators/v1.schema.json`),
//! and the rules around it are those on "SLCR — Tools: Manifest,
//! Descriptors, and Invocation". [file] reads one file against the schema;
//! [vet] checks its `x-vetInvariants`, which need the rest of the project.

pub(crate) use file::{ValidatorFile, is_validator_file};
pub(crate) use problem::{Finding, Problem};

/// One validator file: its model, and loading it.
pub(crate) mod file;
/// Where each validator is in a validator file's source.
mod locate;
/// What can be wrong with a validator.
pub(crate) mod problem;
/// The invariants a validator file's schema can't express.
pub(crate) mod vet;
