//! A CLI tool that introspects Neo4j databases to generate human-readable
//! schema

mod renderer;

use anyhow::{Context, Result};
use clap::Parser;
use futures::future;
use indexmap::IndexSet;
use itertools::Itertools;
use neo4rs::{Config, ConfigBuilder, Graph, query};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    str::FromStr,
};

use crate::renderer::{MarkdownRenderer, SchemaRenderer};

/// Command-line arguments accepted by the schema introspection tool.
///
/// All connection parameters have sensible defaults that match a stock local
/// Neo4j installation so the tool works out-of-the-box without any flags.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Bolt URI of the target instance.
    ///
    /// Accepted formats: `host:port`, `bolt://host:port`, or
    /// `neo4j://host:port`. The driver normalises all three internally.
    #[arg(long, default_value = "127.0.0.1:7687")]
    uri: String,

    /// Neo4j username used for authentication.
    #[arg(long, default_value = "neo4j")]
    user: String,

    /// Neo4j password used for authentication.
    ///
    /// Can also be supplied via the `NEO4J_PASSWORD` environment variable
    /// to avoid exposing credentials in shell history.
    #[arg(long, env = "NEO4J_PASSWORD", default_value = "neo4j")]
    password: String,

    /// Name of the target database within the Neo4j instance.
    #[arg(long, default_value = "neo4j")]
    database: String,

    /// Skip per-property example / min-max queries.
    ///
    /// When set, only type information is printed. This significantly reduces
    /// the number of Cypher round-trips and is useful for large databases
    /// where annotation queries would be too slow.
    #[arg(long, action)]
    no_examples: bool,

    /// Only index properties under the most specific (last) label in a
    /// taxonomy.
    ///
    /// When set, if a node has multiple labels like [:Animal:Mammal:Dog],
    /// properties will only be documented under 'Dog'.
    #[arg(long, action)]
    most_specific: bool,
}

/// Raw type metadata for a single property as returned by the schema
/// introspection procedures.
///
/// A property is keyed by its name within a parent label or relationship type.
/// The `types` list reflects what Neo4j reports via `db.schema.*` procedures;
/// it may contain more than one entry when different nodes store the same
/// property under different Cypher types.
#[derive(Debug, Clone)]
struct PropInfo {
    /// The Cypher types detected for this property across all stored nodes or
    /// relationships (e.g. `["STRING NOT NULL"]` or `["INTEGER", "STRING"]`).
    types: IndexSet<String>,

    /// Indicates whether this property is present on all instances of the
    /// associated node or relationship type.
    required: bool,
}

/// Supplemental data extracted from live records to enrich a property entry.
///
/// Annotations are fetched with additional Cypher queries after the schema
/// procedures have run. They turn a dry type listing into documentation that
/// reflects the actual content of the database.
#[derive(Debug, Clone)]
enum Annotation {
    /// A single representative value sampled from the first matching record.
    ///
    /// Used for `STRING` and `LIST` properties where a concrete example is
    /// more informative than a numeric range.
    Example(String),

    /// The inclusive lower and upper bounds observed across all non-null
    /// values of the property.
    ///
    /// Used for numeric, boolean, and temporal types where the value space
    /// is ordered and a range communicates the data distribution clearly.
    MinMax(String, String),

    /// No annotation is available or applicable for this property.
    ///
    /// Produced for opaque types (`POINT`, `MAP`, `null`), when
    /// `--no-examples` is set, or when every stored value is `null`.
    None,
}

/// A fully-resolved property entry ready to be handed to a renderer.
///
/// It combines the raw schema information from [`PropInfo`] with an
/// [`Annotation`] fetched from live data, so the rendering layer does not
/// need to perform any additional I/O or computation.
#[derive(Debug, Clone)]
struct ResolvedProp {
    /// The property name as it appears in the graph (e.g. `"createdAt"`).
    name: String,

    /// Human-readable type string joining all detected Cypher types with
    /// ` | ` (e.g. `"STRING NOT NULL"` or `"INTEGER | STRING"`).
    type_str: String,

    /// Indicates whether this property is present on all instances of the
    /// associated node or relationship type.
    required: bool,

    /// The annotation fetched for this property, or [`Annotation::None`]
    /// when unavailable.
    annotation: Annotation,
}

/// The base Cypher data type of a Neo4j property value.
///
/// All variants correspond to the type names reported by the schema
/// introspection procedures after stripping the optional ` NOT NULL` suffix.
/// List and map types are collapsed to their container variant regardless of
/// their element type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DataType {
    /// `INTEGER` / `INT` — signed 64-bit whole number.
    Integer,
    /// `FLOAT` — 64-bit IEEE 754 floating-point number.
    Float,
    /// `BOOLEAN` — logical value representing `true` or `false`.
    Boolean,
    /// `DATE` — calendar date without time or timezone (ISO 8601).
    Date,
    /// `DURATION` — ISO 8601 duration (years, months, days, seconds…).
    Duration,
    /// `ZONED DATETIME` — instant in time with a named timezone offset.
    ZonedDatetime,
    /// `DATETIME` — date and time without timezone information.
    Datetime,
    /// `ZONED TIME` — time of day with a timezone offset.
    ZonedTime,
    /// `TIME` — time of day without timezone information.
    Time,
    /// `LOCAL DATETIME` — alias for a datetime local to the query context.
    LocalDatetime,
    /// `LOCAL TIME` — alias for a time local to the query context.
    LocalTime,
    /// `STRING` — UTF-8 text of arbitrary length.
    String,
    /// `POINT` — 2-D or 3-D spatial coordinate (Cartesian or WGS-84).
    Point,
    /// `null` — property exists in the schema but has no concrete type yet.
    Null,
    /// `LIST` — ordered, heterogeneous collection of values.
    List,
    /// `MAP` — key-value collection (nested properties or untyped objects).
    Map,
}

/// Sentinel error returned when a raw type string does not match
/// any known Neo4j data type.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UnknownType;

impl fmt::Display for UnknownType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown Neo4j data type")
    }
}

impl Error for UnknownType {}

impl FromStr for DataType {
    type Err = UnknownType;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.strip_suffix(" NOT NULL").unwrap_or(s) {
            "INTEGER" | "INT" => Ok(Self::Integer),
            "FLOAT" => Ok(Self::Float),
            "BOOLEAN" | "BOOL" => Ok(Self::Boolean),
            "DATE" => Ok(Self::Date),
            "DURATION" => Ok(Self::Duration),
            "ZONED DATETIME" => Ok(Self::ZonedDatetime),
            "DATETIME" => Ok(Self::Datetime),
            "ZONED TIME" => Ok(Self::ZonedTime),
            "TIME" => Ok(Self::Time),
            "LOCAL DATETIME" => Ok(Self::LocalDatetime),
            "LOCAL TIME" => Ok(Self::LocalTime),
            "STRING" => Ok(Self::String),
            "POINT" => Ok(Self::Point),
            "null" => Ok(Self::Null),
            s if s.starts_with("LIST") => Ok(Self::List),
            s if s.starts_with("MAP") => Ok(Self::Map),
            _ => Err(UnknownType),
        }
    }
}

impl DataType {
    /// Returns `true` for ordered types where a min/max range is more
    /// informative than a single representative example.
    ///
    /// Covers all numeric, boolean, and temporal variants. For these types
    /// the annotation query uses `min()` / `max()` aggregates instead of
    /// `LIMIT 1`.
    pub fn is_range_type(&self) -> bool {
        matches!(
            self,
            Self::Integer
                | Self::Float
                | Self::Boolean
                | Self::Date
                | Self::Duration
                | Self::ZonedDatetime
                | Self::Datetime
                | Self::ZonedTime
                | Self::Time
                | Self::LocalDatetime
                | Self::LocalTime
        )
    }
}

/// Fetches a supplemental annotation for a single property by running a
/// targeted Cypher query against live data.
///
/// The query strategy depends on `ptype`:
/// - **Opaque types** (`POINT`, `MAP`, `null`): returns [`Annotation::None`]
///   immediately without touching the database.
/// - **Range types** (numeric, boolean, temporal): runs a `min()` / `max()`
///   aggregate. Zero rows is a normal condition meaning every stored value is
///   `null`; the result is [`Annotation::None`].
/// - **All other types**: runs a `LIMIT 1` query. Zero rows is a normal
///   condition meaning every stored value is `null`; the result is
///   [`Annotation::None`].
///
/// Real query or network failures are propagated so the caller can decide
/// whether to abort or continue.
async fn annotate(
    graph: &Graph,
    match_clause: String,
    prop: &str,
    ptype: DataType,
) -> Result<Annotation> {
    if ptype == DataType::Null {
        return Ok(Annotation::None);
    }

    if ptype.is_range_type() {
        let q = format!(
            "{match_clause} WHERE target.`{prop}` IS NOT NULL \
             RETURN toString(min(target.`{prop}`)) AS mn, \
                    toString(max(target.`{prop}`)) AS mx"
        );
        return Ok(
            if let Some(row) = graph.execute(query(&q)).await?.next().await?
                && let Some(mn) = row.get("mn").ok()
                && let Some(mx) = row.get("mx").ok()
            {
                Annotation::MinMax(mn, mx)
            } else {
                Annotation::None
            },
        );
    }

    let q = format!(
        "{match_clause} WHERE target.`{prop}` IS NOT NULL \
             RETURN target.`{prop}` AS v LIMIT 1"
    );

    Ok(
        if let Some(row) = graph.execute(query(&q)).await?.next().await?
            && let Some(v) = row.get::<serde_json::Value>("v").ok()
        {
            Annotation::Example(v.to_string())
        } else {
            Annotation::None
        },
    )
}

/// Queries `db.schema.nodeTypeProperties()` and returns a two-level map of
/// `label → property name → PropInfo`.
///
/// A single procedure row can list several labels simultaneously (Neo4j
/// groups properties shared by label combinations). Each label gets its own
/// entry so the output map is fully denormalised and easy to iterate.
///
/// When `most_specific` is `true`, only the last label returned in the
/// `nodeLabels` list (representing the most refined sub-class in a taxonomy)
/// is retained, and all more general super-class labels are discarded.
async fn fetch_node_props(
    graph: &Graph,
    most_specific: bool,
) -> Result<BTreeMap<String, BTreeMap<String, PropInfo>>> {
    let mut node_props: BTreeMap<String, BTreeMap<String, PropInfo>> =
        BTreeMap::new();

    let mut result = graph
        .execute(query(
            "CALL db.schema.nodeTypeProperties() \
             YIELD nodeLabels,                   \
                   propertyName,                 \
                   propertyTypes,                \
                   mandatory                     \
             RETURN nodeLabels    AS Label,      \
                    propertyName  AS Property,   \
                    propertyTypes AS Type,       \
                    mandatory     AS Required    \
             ORDER BY Label, Property",
        ))
        .await
        .context("Failed to run nodeTypeProperties")?;

    while let Some(row) = result.next().await? {
        let mut labels: Vec<String> = row.get("Label").unwrap_or_default();
        let prop_name: Option<String> = row.get("Property")?;
        let types: IndexSet<String> = row.get("Type").unwrap_or_default();
        let required: bool = row.get("Required")?;

        let Some(name) = prop_name else { continue };

        if most_specific && let Some(label) = labels.pop() {
            labels = vec![label];
        }

        for label in labels {
            node_props
                .entry(label)
                .or_default()
                .entry(name.clone())
                .and_modify(|info| {
                    info.required &= required;
                    info.types.extend(types.clone());
                })
                .or_insert_with(|| PropInfo {
                    types: types.clone(),
                    required,
                });
        }
    }

    Ok(node_props)
}

/// Queries `db.schema.relTypeProperties()` and returns a two-level map of
/// `relationship type → property name → PropInfo`.
///
/// The relationship type string is used verbatim as returned by the procedure
/// (e.g. `` :`ACTED_IN` ``), which is already valid Cypher syntax for
/// embedding in a relationship pattern such as
/// `MATCH ()-[target:`ACTED_IN`]->()`.
///
/// When `nodes` is `Some`, the returned patterns are filtered to ensure that
/// both the start and end node labels exist within the provided map. This
/// prevents generic super-class relationships from referencing orphaned
/// labels that were excluded during strict taxonomy resolution.
async fn fetch_rel_props(
    graph: &Graph,
) -> Result<BTreeMap<String, BTreeMap<String, PropInfo>>> {
    let mut rel_props: BTreeMap<String, BTreeMap<String, PropInfo>> =
        BTreeMap::new();

    let mut result = graph
        .execute(query(
            "CALL db.schema.relTypeProperties()    \
             YIELD relType,                        \
                   propertyName,                   \
                   propertyTypes,                  \
                   mandatory                       \
             RETURN relType       AS Relationship, \
                    propertyName  AS Property,     \
                    propertyTypes AS Type,         \
                    mandatory     AS Required      \
             ORDER BY relType, Property",
        ))
        .await
        .context("Failed to run relTypeProperties")?;

    while let Some(row) = result.next().await? {
        let rel_type: String = row.get("Relationship").unwrap_or_default();
        let prop_name: Option<String> = row.get("Property")?;
        let types: IndexSet<String> = row.get("Type").unwrap_or_default();
        let required: bool = row.get("Required")?;

        if let Some(name) = prop_name {
            rel_props
                .entry(rel_type)
                .or_default()
                .entry(name.clone())
                .and_modify(|info| {
                    info.required &= required;
                    info.types.extend(types.clone());
                })
                .or_insert_with(|| PropInfo {
                    types: types.clone(),
                    required,
                });
        }
    }

    Ok(rel_props)
}

/// Queries `db.schema.visualization()` and returns the set of directed edge
/// patterns that describe the graph topology.
///
/// Each element of the returned set is a `(start labels, relationship type,
/// end labels)` triple. When a node carries multiple labels they are joined
/// with `:` (e.g. `"Person:Actor"`).
///
/// When `nodes` is `Some`
async fn fetch_patterns(
    graph: &Graph,
    nodes: Option<&BTreeMap<String, Vec<ResolvedProp>>>,
) -> Result<BTreeSet<(String, String, String)>> {
    let mut patterns: BTreeSet<(String, String, String)> = BTreeSet::new();

    let mut result = graph
        .execute(query(
            "CALL db.schema.visualization()    \
             YIELD relationships               \
             UNWIND relationships AS rel       \
             WITH startNode(rel) AS s,         \
                  type(rel)      AS t,         \
                  endNode(rel)   AS e          \
             RETURN labels(s) AS StartNode,    \
                    t         AS Relationship, \
                    labels(e) AS EndNode",
        ))
        .await
        .context("Failed to run schema visualization")?;

    while let Some(row) = result.next().await? {
        let mut start: Vec<String> = row.get("StartNode").unwrap_or_default();
        let rel: String = row.get("Relationship").unwrap_or_default();
        let mut end: Vec<String> = row.get("EndNode").unwrap_or_default();

        if let Some(nodes) = nodes {
            start.retain(|s| nodes.contains_key(s));
            end.retain(|s| nodes.contains_key(s));
        }

        if !start.is_empty() && !end.is_empty() {
            patterns.insert((start.join(":"), rel, end.join(":")));
        }
    }

    Ok(patterns)
}

/// Resolves all properties in `entity_props` into [`ResolvedProp`] entries
/// by fetching their annotations concurrently.
///
/// `match_clause_fn` is a closure that, given an entity name (label or
/// relationship type), returns the Cypher `MATCH` clause used by annotation
/// queries. The aliased variable **must** always be named `target` because
/// that is the name the annotation queries refer to.
///
/// All annotation futures are submitted simultaneously with
/// [`future::try_join_all`], so total wait time is bounded by the slowest
/// single query rather than the sum of all queries. A driver or network
/// failure in any one future causes the whole function to return that error
/// immediately.
///
/// When `no_examples` is `true` every property receives [`Annotation::None`]
/// without any additional database traffic.
async fn resolve_annotations(
    graph: &Graph,
    entity_props: &BTreeMap<String, BTreeMap<String, PropInfo>>,
    match_clause_fn: impl Fn(&str) -> String,
    no_examples: bool,
) -> Result<BTreeMap<String, Vec<ResolvedProp>>> {
    /// Flat record collecting everything needed to fire one annotation query
    /// and later reassemble the result into the grouped output map.
    /// Fields:
    /// (
    ///     entity name,
    ///     property name,
    ///     formatted type string,
    ///     is required,
    ///     primary type
    /// ).
    type TaskMeta = (String, String, String, bool, DataType);

    let tasks: Vec<TaskMeta> = entity_props
        .iter()
        .flat_map(|(label, props)| {
            props.iter().filter_map(|(prop_name, prop)| {
                let primary: DataType = prop.types.first()?.parse().ok()?;
                Some((
                    label.clone(),
                    prop_name.clone(),
                    prop.types.iter().join(" | "),
                    prop.required,
                    primary,
                ))
            })
        })
        .collect();

    let annotations: Vec<Annotation> = if no_examples {
        vec![Annotation::None; tasks.len()]
    } else {
        future::try_join_all(tasks.iter().map(
            |(label, prop_name, _, _, dt)| {
                annotate(graph, match_clause_fn(label), prop_name, *dt)
            },
        ))
        .await?
    };

    let mut output: BTreeMap<String, Vec<ResolvedProp>> = BTreeMap::new();

    for ((label, prop_name, type_str, required, _), ann) in
        tasks.into_iter().zip(annotations)
    {
        output.entry(label).or_default().push(ResolvedProp {
            name: prop_name,
            type_str,
            required,
            annotation: ann,
        });
    }

    Ok(output)
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

    let (nodes, rels): (
        BTreeMap<String, Vec<ResolvedProp>>,
        BTreeMap<String, Vec<ResolvedProp>>,
    ) = tokio::try_join!(
        async {
            resolve_annotations(
                &graph,
                &fetch_node_props(&graph, args.most_specific).await?,
                |label| format!("MATCH (target:`{label}`)"),
                args.no_examples,
            )
            .await
        },
        async {
            resolve_annotations(
                &graph,
                &fetch_rel_props(&graph).await?,
                |label| format!("MATCH ()-[target{label}]->()"),
                args.no_examples,
            )
            .await
        },
    )
    .context("Failed to retrieve information")?;

    let patterns: BTreeSet<(String, String, String)> =
        fetch_patterns(&graph, args.most_specific.then_some(&nodes))
            .await
            .context("Failed to retrieve information")?;

    MarkdownRenderer
        .render(&nodes, &rels, &patterns)
        .context("Failed to print the schema")?;

    Ok(())
}

