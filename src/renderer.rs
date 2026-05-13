//! Neo4j schema rendering abstractions and implementations.
//!
//! This module defines the contract and provided implementations for
//! formatting and emitting a fully-resolved Neo4j graph schema to an output
//! medium.

use std::collections::{BTreeMap, BTreeSet};

use crate::{Annotation, ResolvedProp};

/// Renders a fully-resolved Neo4j schema to some output medium.
///
/// Implementors receive the complete, pre-computed schema data and are
/// responsible for all formatting and I/O. Because data collection is
/// handled upstream, a renderer never needs to perform network calls.
///
/// The three parameters map directly to the three logical sections of the
/// output: node properties grouped by label, relationship properties grouped
/// by type, and the set of directed edge patterns that describe the graph
/// topology.
pub trait SchemaRenderer {
    /// Formats and emits the schema described by `nodes`, `rels`, and
    /// `patterns` to the renderer's target output.
    ///
    /// Returns an error if the underlying I/O operation fails (e.g. a broken
    /// pipe when piping output to another process, or a write failure when
    /// targeting a file).
    fn render(
        &self,
        nodes: &BTreeMap<String, Vec<ResolvedProp>>,
        rels: &BTreeMap<String, Vec<ResolvedProp>>,
        patterns: &BTreeSet<(String, String, String)>,
    ) -> anyhow::Result<()>;
}

/// A [`SchemaRenderer`] that writes Markdown-flavoured plain text to stdout.
///
/// Node and relationship sections use bold labels as list headers, with each
/// property rendered as an indented code-formatted entry followed by its type
/// string and optional annotation. The topology section prints one Cypher
/// pattern per line.
pub struct MarkdownRenderer;

impl MarkdownRenderer {
    /// Builds the annotation suffix appended after the type string for a
    /// property line.
    ///
    /// The format depends on the annotation variant:
    /// - [`Annotation::Example`] with a `STRING` type wraps the value in
    ///   quotes for readability.
    /// - [`Annotation::Example`] with any other type prints the value bare.
    /// - [`Annotation::MinMax`] prints `Min: …, Max: …`.
    /// - [`Annotation::None`] returns an empty string so the caller's
    ///   `format!` call adds nothing.
    fn annotation_suffix(ann: &Annotation) -> String {
        match ann {
            Annotation::Example(ex) => {
                format!(" Example: {}", ex)
            }
            Annotation::MinMax(mn, mx) => format!(" Min: {}, Max: {}", mn, mx),
            Annotation::None => String::new(),
        }
    }
}

impl SchemaRenderer for MarkdownRenderer {
    fn render(
        &self,
        nodes: &BTreeMap<String, Vec<ResolvedProp>>,
        rels: &BTreeMap<String, Vec<ResolvedProp>>,
        patterns: &BTreeSet<(String, String, String)>,
    ) -> anyhow::Result<()> {
        println!("Node properties:");
        for (label, props) in nodes {
            println!("- **{label}**");
            for p in props {
                println!(
                    "  - `{}`: {}{}{}",
                    p.name,
                    p.type_str,
                    if p.required { " REQUIRED" } else { "" },
                    Self::annotation_suffix(&p.annotation)
                );
            }
        }

        println!("\nRelationship properties:");
        for (rel, props) in rels {
            println!("- **{rel}**");
            for p in props {
                println!(
                    "  - `{}`: {}{}{}",
                    p.name,
                    p.type_str,
                    if p.required { " REQUIRED" } else { "" },
                    Self::annotation_suffix(&p.annotation)
                );
            }
        }

        println!("\nThe relationships:");
        for (start, rel, end) in patterns {
            println!("(:{start})-([:{}])-> (:{end})", rel);
        }

        Ok(())
    }
}
