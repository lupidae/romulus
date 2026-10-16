//! Objects an extension creates. remus lists them like any other (PostGIS's
//! `spatial_ref_sys` is a table to the catalog), but they belong to the
//! extension: `CREATE EXTENSION` makes them and `DROP EXTENSION` takes them
//! away, so a migration must neither create nor drop them.
//!
//! The list covers the extensions the sandbox offers; the oracle checks it
//! against what the sandbox measures (`romulus_sandbox::catalog`).

use remus_core::Schema;

/// Relations (tables, views, sequences) and types, by extension.
pub const PROVIDED: &[(&str, &str)] = &[
    ("earthdistance", "earth"),
    ("postgis", "spatial_ref_sys"),
    ("postgis", "geography_columns"),
    ("postgis", "geometry_columns"),
    ("postgis", "geometry_dump"),
    ("postgis", "valid_detail"),
    ("postgis_topology", "topology_id_seq"),
    ("postgis_topology", "topology"),
    ("postgis_topology", "layer"),
    ("postgis_topology", "topogeometry"),
    ("postgis_topology", "topoelement"),
    ("postgis_topology", "topoelementarray"),
    ("postgis_topology", "getfaceedges_returntype"),
    ("postgis_topology", "validatetopology_returntype"),
];

/// Extensions that need another one installed first.
pub const REQUIRES: &[(&str, &str)] = &[
    ("earthdistance", "cube"),
    ("postgis_raster", "postgis"),
    ("postgis_sfcgal", "postgis"),
    ("postgis_tiger_geocoder", "fuzzystrmatch"),
    ("postgis_tiger_geocoder", "postgis"),
    ("postgis_topology", "postgis"),
];

/// Extension names in an order where each comes after those it requires
/// (then by name).
pub(crate) fn install_order<'n>(names: &[&'n str]) -> Vec<&'n str> {
    let mut done: Vec<&str> = Vec::with_capacity(names.len());
    let mut left: Vec<&str> = names.to_vec();
    left.sort_unstable();
    while !left.is_empty() {
        let ready = left
            .iter()
            .position(|n| {
                REQUIRES
                    .iter()
                    .filter(|(x, _)| x == n)
                    .all(|(_, required)| !left.contains(required) || done.contains(required))
            })
            .unwrap_or(0);
        done.push(left.remove(ready));
    }
    done
}

/// `schema` without the objects its extensions provide.
pub(crate) fn without_provided(schema: &mut Schema) {
    let provided = |object_schema: &str, name: &str| {
        schema.extensions.iter().any(|x| {
            x.schema == object_schema
                && PROVIDED
                    .iter()
                    .any(|(extension, object)| *extension == x.name && *object == name)
        })
    };
    let entities: Vec<bool> = schema
        .entities
        .iter()
        .map(|e| provided(&e.schema, &e.name))
        .collect();
    let enums: Vec<bool> = schema
        .enums
        .iter()
        .map(|e| provided(&e.schema, &e.name))
        .collect();
    let domains: Vec<bool> = schema
        .domains
        .iter()
        .map(|d| provided(&d.schema, &d.name))
        .collect();
    let composites: Vec<bool> = schema
        .composites
        .iter()
        .map(|c| provided(&c.schema, &c.name))
        .collect();
    let mut keep = entities.into_iter().map(|p| !p);
    schema.entities.retain(|_| keep.next().unwrap_or(true));
    let mut keep = enums.into_iter().map(|p| !p);
    schema.enums.retain(|_| keep.next().unwrap_or(true));
    let mut keep = domains.into_iter().map(|p| !p);
    schema.domains.retain(|_| keep.next().unwrap_or(true));
    let mut keep = composites.into_iter().map(|p| !p);
    schema.composites.retain(|_| keep.next().unwrap_or(true));
}
