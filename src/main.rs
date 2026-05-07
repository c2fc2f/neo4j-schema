//! A CLI tool that introspects Neo4j databases to generate human-readable
//! schema

use anyhow::{Context, Result};
use clap::Parser;
use neo4rs::{Config, ConfigBuilder, Graph, query};
use std::collections::{BTreeMap, BTreeSet};

/// A CLI tool that introspects Neo4j databases to generate human-readable
/// schema
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Bolt URI — host:port, bolt://host:port, or neo4j://host:port
    #[arg(long, default_value = "127.0.0.1:7687")]
    uri: String,

    /// Neo4j username
    #[arg(long, default_value = "neo4j")]
    user: String,

    /// Neo4j password (also read from NEO4J_PASSWORD env var)
    #[arg(long, env = "NEO4J_PASSWORD", default_value = "neo4j")]
    password: String,

    /// Target database name
    #[arg(long, default_value = "neo4j")]
    database: String,

    /// Skip per-property example / min-max queries (faster on large DBs)
    #[arg(long, default_value_t = false)]
    no_examples: bool,
}

/// Metadata for a specific property belonging to a node label or relationship
/// type.
#[derive(Debug, Clone)]
struct PropInfo {
    /// The name of the property.
    name: String,
    /// The Cypher types detected for this property.
    /// Neo4j properties can technically contain multiple types across
    /// different records.
    types: Vec<String>,
}

/// Represents supplemental data extracted from the database to give context
/// to a property.
///
/// This is used to enrich the schema output with real-world data points,
/// making the generated documentation more informative than just a list of
/// types.
enum Annotation {
    /// A single representative value sampled from the database.
    Example(String),
    /// The lower and upper bounds of the data (used for numeric, date, or
    /// duration types).
    MinMax(String, String),
    /// No supplemental data available or requested.
    None,
}

/// Types where min/max is more informative than a single example.
/// (Will break in Cypher 25)
fn is_range_type(t: &str) -> bool {
    matches!(
        t,
        "Long"
            | "Double"
            | "LocalDate"
            | "ZonedDateTime"
            | "LocalDateTime"
            | "LocalTime"
            | "OffsetTime"
            | "Duration"
    )
}

/// Types that carry no useful text annotation.
/// (Will break in Cypher 25)
fn is_opaque_type(t: &str) -> bool {
    matches!(t, "Point" | "null" | "List" | "Map")
}

/// Build the annotation suffix that follows the type name in the output.
fn annotation_suffix(ann: Annotation, ptype: &str) -> String {
    match ann {
        Annotation::Example(ex) => {
            if ptype == "String" || ptype.is_empty() {
                format!(" Example: \"{}\"", ex)
            } else {
                format!(" Example: {}", ex)
            }
        }
        Annotation::MinMax(mn, mx) => format!(" Min: {}, Max: {}", mn, mx),
        Annotation::None => String::new(),
    }
}

/// Build the annotation suffix that follows the type name in the output.
async fn annotate_node(
    graph: &Graph,
    label: &str,
    prop: &str,
    ptype: &str,
) -> Annotation {
    if is_opaque_type(ptype) {
        return Annotation::None;
    }

    if is_range_type(ptype) {
        let q: String = format!(
            "MATCH (n:`{label}`) WHERE n.`{prop}` IS NOT NULL \
             RETURN toString(min(n.`{prop}`)) AS mn, \
                    toString(max(n.`{prop}`)) AS mx"
        );
        if let Ok(mut res) = graph.execute(query(&q)).await
            && let Ok(Some(row)) = res.next().await
        {
            let mn: Option<String> = row.get("mn").ok();
            let mx: Option<String> = row.get("mx").ok();
            if let (Some(mn), Some(mx)) = (mn, mx) {
                return Annotation::MinMax(mn, mx);
            }
        }
    } else {
        let q: String = format!(
            "MATCH (n:`{label}`) WHERE n.`{prop}` IS NOT NULL \
             RETURN toString(n.`{prop}`) AS v LIMIT 1"
        );
        if let Ok(mut res) = graph.execute(query(&q)).await
            && let Ok(Some(row)) = res.next().await
            && let Ok(v) = row.get::<String>("v")
        {
            return Annotation::Example(v);
        }
    }

    Annotation::None
}

/// Build the annotation suffix that follows the type name in the output.
async fn annotate_rel(
    graph: &Graph,
    rel_type: &str,
    prop: &str,
    ptype: &str,
) -> Annotation {
    if is_opaque_type(ptype) {
        return Annotation::None;
    }

    if is_range_type(ptype) {
        let q: String = format!(
            "MATCH ()-[r:`{rel_type}`]->() WHERE r.`{prop}` IS NOT NULL \
             RETURN toString(min(r.`{prop}`)) AS mn, \
                    toString(max(r.`{prop}`)) AS mx"
        );
        if let Ok(mut res) = graph.execute(query(&q)).await
            && let Ok(Some(row)) = res.next().await
        {
            let mn: Option<String> = row.get("mn").ok();
            let mx: Option<String> = row.get("mx").ok();
            if let (Some(mn), Some(mx)) = (mn, mx) {
                return Annotation::MinMax(mn, mx);
            }
        }
    } else {
        let q: String = format!(
            "MATCH ()-[r:`{rel_type}`]->() WHERE r.`{prop}` IS NOT NULL \
             RETURN toString(r.`{prop}`) AS v LIMIT 1"
        );
        if let Ok(mut res) = graph.execute(query(&q)).await
            && let Ok(Some(row)) = res.next().await
            && let Ok(v) = row.get::<String>("v")
        {
            return Annotation::Example(v);
        }
    }

    Annotation::None
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Args = Args::parse();

    let config: Config = ConfigBuilder::default()
        .uri(&args.uri)
        .user(&args.user)
        .password(&args.password)
        .db(args.database)
        .build()
        .context("Failed to build Neo4j config")?;

    let graph: Graph = Graph::connect(config)
        .await
        .context("Failed to connect to Neo4j")?;

    // label -> prop_name -> PropInfo
    let mut node_props: BTreeMap<String, BTreeMap<String, PropInfo>> =
        BTreeMap::new();

    let mut result = graph
        .execute(query(
            "CALL db.schema.nodeTypeProperties() \
             YIELD nodeLabels, propertyName, propertyTypes, mandatory \
             RETURN nodeLabels   AS Label,    \
                    propertyName  AS Property, \
                    propertyTypes AS Type,     \
                    mandatory     AS Required  \
             ORDER BY Label, Property",
        ))
        .await
        .context("Failed to run nodeTypeProperties")?;

    while let Ok(Some(row)) = result.next().await {
        let labels: Vec<String> = row.get("Label").unwrap_or_default();
        let prop_name: Option<String> = row.get("Property").ok();
        let types: Vec<String> = row.get("Type").unwrap_or_default();

        let Some(name) = prop_name else { continue };

        for label in labels {
            node_props
                .entry(label)
                .or_default()
                .entry(name.clone())
                .or_insert(PropInfo {
                    name: name.clone(),
                    types: types.clone(),
                });
        }
    }

    let mut rel_props: BTreeMap<String, BTreeMap<String, PropInfo>> =
        BTreeMap::new();

    let mut result = graph
        .execute(query(
            "CALL db.schema.relTypeProperties() \
             YIELD relType, propertyName, propertyTypes, mandatory \
             RETURN relType       AS Relationship, \
                    propertyName  AS Property,     \
                    propertyTypes AS Type,          \
                    mandatory     AS IsRequired     \
             ORDER BY relType, Property",
        ))
        .await
        .context("Failed to run relTypeProperties")?;

    while let Ok(Some(row)) = result.next().await {
        let rel_type: String = row.get("Relationship").unwrap_or_default();
        let prop_name: Option<String> = row.get("Property").ok();
        let types: Vec<String> = row.get("Type").unwrap_or_default();

        if let Some(name) = prop_name {
            rel_props
                .entry(rel_type)
                .or_default()
                .entry(name.clone())
                .or_insert(PropInfo {
                    name: name.clone(),
                    types,
                });
        } else {
            rel_props.entry(rel_type).or_default();
        }
    }

    let mut patterns: BTreeSet<(String, String, String)> = BTreeSet::new();

    let mut result = graph
        .execute(query(
            "CALL db.schema.visualization() \
             YIELD relationships \
             UNWIND relationships AS rel \
             WITH startNode(rel) AS s, type(rel) AS t, endNode(rel) AS e \
             RETURN labels(s) AS StartNode, t AS Relationship, labels(e) AS EndNode",
        ))
        .await
        .context("Failed to run schema visualization")?;

    while let Ok(Some(row)) = result.next().await {
        let start: Vec<String> = row.get("StartNode").unwrap_or_default();
        let rel: String = row.get("Relationship").unwrap_or_default();
        let end: Vec<String> = row.get("EndNode").unwrap_or_default();
        patterns.insert((start.join(":"), rel, end.join(":")));
    }

    println!("Node properties:");
    for (label, props) in &node_props {
        println!("- **{}**", label);
        for prop in props.values() {
            let type_str: String = prop.types.join(" | ");
            let primary: &str =
                prop.types.first().map(String::as_str).unwrap_or("");

            let suffix: String = if args.no_examples {
                String::new()
            } else {
                let ann: Annotation =
                    annotate_node(&graph, label, &prop.name, primary).await;
                annotation_suffix(ann, primary)
            };

            println!("  - `{}`: {}{}", prop.name, type_str, suffix);
        }
    }

    println!();
    println!("Relationship properties:");
    for (rel_type, props) in &rel_props {
        if props.is_empty() {
            continue;
        }
        println!("- **{}**", rel_type);
        for prop in props.values() {
            let type_str: String = prop.types.join(" | ");
            let primary: &str =
                prop.types.first().map(String::as_str).unwrap_or("");

            let suffix: String = if args.no_examples {
                String::new()
            } else {
                let ann: Annotation =
                    annotate_rel(&graph, rel_type, &prop.name, primary).await;
                annotation_suffix(ann, primary)
            };

            println!("  - `{}`: {}{}", prop.name, type_str, suffix);
        }
    }

    println!();
    println!("The relationships:");
    for (start, rel, end) in &patterns {
        println!("(:{})-([:{}])->(:{})", start, rel, end);
    }

    Ok(())
}
