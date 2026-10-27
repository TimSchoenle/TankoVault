//! The Manganato network: one layout served from several domains.

use serde_json::json;
use tankovault_domain::{AdapterKind, Politeness};

use super::{BuiltinPreset, bulk_budget};

/// The Manganato clone family — three domains serving the same application, each its own
/// provider because each has its own rate limit, health state and reader-facing links.
pub(super) fn manganato_family() -> Vec<BuiltinPreset> {
    vec![
        // Three domains serving the same application; each is its own provider because each
        // has its own rate limit, health state and reader-facing links.
        BuiltinPreset {
            slug: "natomanga",
            name: "NatoManga",
            base_url: "https://www.natomanga.com",
            adapter: AdapterKind::Manganato,
            config: json!({}),
            // ~89k series across ~3.7k catalogue pages, and Cloudflare in front: sized like
            // kunmanga rather than the default. Per worker *process*, so at two replicas this
            // is 8 rps aggregate — the policy ceiling.
            politeness: Politeness {
                rps: 4.0,
                concurrency: 8,
                ..Politeness::default()
            },
        },
        BuiltinPreset {
            slug: "mangakakalot",
            name: "Mangakakalot",
            base_url: "https://www.mangakakalot.gg",
            adapter: AdapterKind::Manganato,
            config: json!({}),
            politeness: Politeness {
                rps: 4.0,
                concurrency: 8,
                ..Politeness::default()
            },
        },
        BuiltinPreset {
            slug: "nelomanga",
            name: "NeloManga",
            base_url: "https://www.nelomanga.net",
            adapter: AdapterKind::Manganato,
            config: json!({}),
            politeness: Politeness {
                rps: 4.0,
                concurrency: 8,
                ..Politeness::default()
            },
        },
        // Two more of the same application found in the 2026-08-26 survey. Sized like their
        // siblings above, and kept separate for the same reason.
        manganato("mangabats", "Mangabat", "https://www.mangabats.com"),
        manganato("manganatogg", "Manganato", "https://manganato.gg"),
        manganato(
            "mangakakalove",
            "MangaKakaLove",
            "https://www.mangakakalove.com",
        ),
    ]
}

/// One domain of the Manganato application, at the catalogue-scale crawl budget the family
/// shares. Per worker *process*, so at the shipped two replicas this is the policy ceiling.
fn manganato(slug: &'static str, name: &'static str, base_url: &'static str) -> BuiltinPreset {
    BuiltinPreset {
        slug,
        name,
        base_url,
        adapter: AdapterKind::Manganato,
        config: json!({}),
        politeness: bulk_budget(),
    }
}
