//! The Discover screen's requests: the tag and provider facets, and one page of results.

use dioxus::prelude::*;
use progenitor_client::ResponseValue;
use tankovault_api_client::Client;

use super::query::{YEAR_MAX, YEAR_MIN};
use super::window::Fetch;
use crate::api::{self, Api};
use crate::i18n::Translator;
use crate::models::*;

/// The tag and provider facets the filter panel offers.
///
/// Degrades to an empty facet on failure rather than blocking the screen — a missing
/// tag/provider filter is cheaper than an error state.
pub(super) fn use_facets(api: Api) -> (Vec<TagFacet>, Vec<PublicProvider>) {
    let tags_res = use_resource(move || {
        let client = api.client();
        async move {
            client
                .tags()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .unwrap_or_default()
        }
    });
    let all_tags: Vec<TagFacet> = tags_res.read_unchecked().clone().unwrap_or_default();

    let providers_res = use_resource(move || {
        let client = api.client();
        async move {
            client
                .providers()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .unwrap_or_default()
        }
    });
    let all_providers: Vec<PublicProvider> =
        providers_res.read_unchecked().clone().unwrap_or_default();
    (all_tags, all_providers)
}

/// One page of results for `request`, paired with the request it answers.
pub(super) async fn page(
    client: &Client,
    request: Fetch,
    i18n: Translator,
) -> (Fetch, Result<SeriesPage, String>) {
    let filters = &request.filters;
    let mut builder = client.list();
    if let Some(content_type) = filters.types.first() {
        builder = builder.content_type(content_type.token());
    }
    if let Some(status) = filters.statuses.first() {
        builder = builder.status(status.token());
    }
    if let Some(provider) = filters.provider.clone() {
        builder = builder.provider(provider);
    }
    if !filters.inc.is_empty() {
        builder = builder.tag(filters.inc.clone());
    }
    if !filters.exc.is_empty() {
        builder = builder.exclude_tag(filters.exc.clone());
    }
    if filters.year_min > YEAR_MIN {
        builder = builder.year_min(filters.year_min);
    }
    if filters.year_max < YEAR_MAX {
        builder = builder.year_max(filters.year_max);
    }
    if filters.min_chapters > 0 {
        builder = builder.min_chapters(filters.min_chapters);
    }
    if let Some(tracking) = filters.tracking.param() {
        builder = builder.tracking(tracking);
    }
    let outcome = builder
        .sort(filters.sort.token())
        .page(i64::try_from(request.page).unwrap_or(i64::MAX))
        .limit(i64::try_from(request.size).unwrap_or(24))
        .with_total(request.with_total)
        .send()
        .await
        .map(|r| {
            let total = header_total(
                r.headers()
                    .get("x-total-count")
                    .and_then(|v| v.to_str().ok()),
            );
            let next_cursor = r
                .headers()
                .get("x-next-cursor")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<i64>().ok());
            SeriesPage {
                items: r.into_inner(),
                total,
                next_cursor,
            }
        })
        .map_err(|e| api::friendly_error(i18n, e));
    // The request travels with its own answer, success or failure. A response that outlived
    // the window it was asked for is then simply not the current one, rather than a page
    // merged into a list it does not belong to.
    (request, outcome)
}

/// The match count a page response reports, if it reports one.
///
/// Absent when the request asked the server not to count, which every page after a window's
/// first does; the window keeps the total its first page reported.
pub(super) fn header_total(value: Option<&str>) -> Option<i64> {
    value.and_then(|v| v.parse::<i64>().ok())
}
