# neo4j-schema

A command-line tool written in Rust that connects to a live [Neo4j](https://neo4j.com/) instance and introspects it to produce a human-readable schema summary: node labels with their property names, types, and sampled values; relationship types with their properties; and the full set of directed edge patterns that describe the graph topology.

## Overview

Neo4j exposes schema metadata through its `db.schema.*` procedures, but the raw output is terse and spread across multiple queries. This tool fires those queries concurrently, correlates the results, enriches each property with a live example value or min/max range, and renders everything as clean Markdown to stdout — ready to paste into documentation, feed to a language model, or diff across schema versions.

The project is a single Cargo package (no workspace). The `neo4j-schema` binary is the sole deliverable; the `renderer` module provides the `SchemaRenderer` trait and its `MarkdownRenderer` implementation.

## Requirements

- Rust toolchain (edition 2024, stable)
- A running Neo4j instance reachable over the Bolt protocol (Cypher 25>=)

## Installation

### From source

```bash
git clone https://github.com/c2fc2f/neo4j-schema
cd neo4j-schema
cargo build --release
# or
cargo install --git https://github.com/c2fc2f/neo4j-schema
```

The compiled binary will be at `target/release/neo4j-schema`.

### With Nix

A Nix flake is provided:

```bash
nix run github:c2fc2f/neo4j-schema -- --help
# or
nix build
# or, to enter a development shell:
nix develop
```

## Usage

```
neo4j-schema [OPTIONS]
```

| Flag | Description | Default |
|---|---|---|
| `--uri <URI>` | Bolt URI of the target instance. Accepted formats: `host:port`, `bolt://host:port`, or `neo4j://host:port` | `127.0.0.1:7687` |
| `--user <USER>` | Neo4j username | `neo4j` |
| `--password <PASSWORD>` | Neo4j password. Can also be supplied via the `NEO4J_PASSWORD` environment variable to avoid exposing credentials in shell history | `neo4j` |
| `--database <DATABASE>` | Name of the target database within the Neo4j instance | `neo4j` |
| `--no-examples` | Skip per-property example / min-max queries. Only type information is printed; significantly reduces the number of Cypher round-trips and is useful for large databases where annotation queries would be too slow | *(disabled)* |
| `--nodes <LABELS>` | Only introspect the specified node labels. Labels are colon-separated (e.g. `Person:Movie`). Topology patterns are constrained to the allowed set — only edges where both endpoints are in the list are emitted | *(all labels)* |
| `--rels <TYPES>` | Only introspect the specified relationship types. Types are colon-separated (e.g. `ACTED_IN:DIRECTED`). Topology patterns are constrained to the allowed set | *(all types)* |

### Examples

Introspect the default local instance:

```bash
neo4j-schema
```

Connect to a remote instance with a custom database, passing the password via the environment:

```bash
NEO4J_PASSWORD=secret neo4j-schema --uri bolt://db.example.com:7687 --database mydb
```

Produce a lightweight schema (types only, no example values) for a large production database:

```bash
neo4j-schema --uri bolt://prod:7687 --no-examples
```

Scope the output to a specific subset of the graph — useful when feeding a focused slice to an LLM:

```bash
neo4j-schema --nodes Person:Movie --rels ACTED_IN:DIRECTED
```

## Output Format

The tool writes Markdown to stdout, divided into three sections.

**Node properties** — one entry per label, listing each property with its Cypher type, whether it is required (present on every node of that label), and either a representative example value or an observed min/max range:

```markdown
Node properties:
- **PubMedArticle**
  - `pmid`: INTEGER NOT NULL REQUIRED Min: 1, Max: 41610285
  - `title`: STRING NOT NULL Example: "Physiological measurements of work stress in medical nursing."
  - `abstract`: STRING NOT NULL Example: "In this study we have investigated …"
  - `dateCompleted`: DATE NOT NULL Min: 1965-11-13, Max: 2026-01-29
  - `dateRevised`: DATE NOT NULL REQUIRED Min: 2000-09-05, Max: 2026-01-29
```

**Relationship properties** — same structure for relationship types that carry their own properties:

```markdown
Relationship properties:
- **`HAS_MESH`**
  - `descriptorIsMajorTopic`: BOOLEAN NOT NULL REQUIRED Min: false, Max: true
  - `qualifierMajorTopics`: LIST<STRING NOT NULL> NOT NULL Example: ["Q000187"]
- **`HAS_CONCEPT`**
  - `isPreferred`: BOOLEAN NOT NULL REQUIRED Min: false, Max: true
```

**The relationships** — one Cypher-style edge pattern per line, describing every possible source label → relationship type → target label combination observed in the graph:

```markdown
The relationships:
(:PubMedArticle)-([:CITES])-> (:PubMedArticle)
(:PubMedArticle)-([:HAS_AUTHOR])-> (:PubMedPerson)
(:PubMedArticle)-([:HAS_MESH])-> (:MeSHDescriptorQualified)
(:MeSHDescriptor)-([:HAS_CONCEPT])-> (:MeSHConcept)
(:MeSHDescriptor)-([:NARROWER_THAN])-> (:MeSHDescriptor)
…
```

## Library Module

The `renderer` module exposes a `SchemaRenderer` trait for anyone who wants to plug in a different output format:

```rust
pub trait SchemaRenderer {
    fn render(
        &self,
        nodes: &BTreeMap<String, Vec<ResolvedProp>>,
        rels:  &BTreeMap<String, Vec<ResolvedProp>>,
        patterns: &BTreeSet<(String, String, String)>,
    ) -> anyhow::Result<()>;
}
```

The provided `MarkdownRenderer` implements this trait and writes to stdout. Custom renderers (JSON, HTML, DOT graph format, …) can be added by implementing the same trait without touching the introspection logic.

## License

This project is licensed under the [MIT License](LICENSE).
