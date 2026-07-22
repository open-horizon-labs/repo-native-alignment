//! Deterministic source projection: select bodies, coalesce overlap, and enforce body caps.

use std::collections::{BTreeMap, btree_map::Entry};

use super::model::*;
use super::source::{SourceError, SourceReader, SourceSlice};

#[derive(Debug, Clone)]
struct Candidate {
    record_id: String,
    selection_rank: usize,
    selection: SelectionSummary,
    language: String,
    slice: SourceSlice,
    representation: BodyRepresentation,
}

pub(crate) fn plan_projection(
    request: ProjectionRequest,
    mut input: ProjectionInput,
    source: &SourceReader,
) -> ProjectionPlan {
    input.records.sort_by(record_order);
    input.candidate_audit.sort_by(candidate_audit_order);
    input.relationships.sort();
    input.capabilities.sort();

    let mut omissions = input.omissions;
    let mut records: Vec<_> = input
        .records
        .iter()
        .map(|selected| projected_record(&request, selected, source, &mut omissions))
        .collect();
    let mut candidates = Vec::new();

    for selected in &input.records {
        if matches!(
            request.body_policy,
            BodyPolicy::SignatureOnly | BodyPolicy::NoBody
        ) {
            if request.body_policy == BodyPolicy::NoBody {
                omit(
                    &mut omissions,
                    selected,
                    None,
                    OmissionCode::NoBodyPolicy,
                    "body policy requested no source body",
                );
            }
            continue;
        }
        let Some(full) = selected.identity.source.as_ref() else {
            omit(
                &mut omissions,
                selected,
                None,
                OmissionCode::MissingSource,
                "record has no source span",
            );
            continue;
        };
        if !full.is_valid() {
            omit(
                &mut omissions,
                selected,
                Some(full.clone()),
                OmissionCode::SourceUnavailable,
                "record source span is invalid",
            );
            continue;
        }
        let focused = selected
            .focused_span
            .as_ref()
            .filter(|span| full.contains(span));
        if selected.focused_span.is_some() && focused.is_none() {
            omit(
                &mut omissions,
                selected,
                selected.focused_span.clone(),
                OmissionCode::InvalidFocusedSpan,
                "focused span is not contained by the record source span",
            );
        }
        let preferred = if request.body_policy == BodyPolicy::FocusedSpan {
            focused.unwrap_or(full)
        } else {
            full
        };
        let mut choices = vec![preferred];
        if preferred == full
            && let Some(focus) = focused
            && focus != full
        {
            choices.push(focus);
        }

        let mut admitted = None;
        let mut used_focused_fallback = false;
        let mut last_error = String::new();
        for choice in choices {
            match source.read(choice) {
                Ok(slice) => {
                    if request
                        .budget
                        .per_record_body_bytes
                        .is_none_or(|cap| slice.text.len() <= cap)
                    {
                        used_focused_fallback =
                            choice != full && request.body_policy != BodyPolicy::FocusedSpan;
                        admitted = Some((choice.clone(), slice));
                        break;
                    }
                    last_error = format!(
                        "source body exceeds the per-record cap of {} bytes",
                        request.budget.per_record_body_bytes.unwrap()
                    );
                }
                Err(error) => last_error = error.to_string(),
            }
        }
        let Some((chosen, slice)) = admitted else {
            let code = if last_error.contains("per-record cap") {
                OmissionCode::PerRecordBodyCap
            } else {
                OmissionCode::SourceUnavailable
            };
            omit(
                &mut omissions,
                selected,
                Some(preferred.clone()),
                code,
                &last_error,
            );
            continue;
        };
        let representation = if chosen != *full {
            BodyRepresentation::FocusedSpan
        } else {
            BodyRepresentation::Complete
        };
        if used_focused_fallback {
            omit(
                &mut omissions,
                selected,
                Some(full.clone()),
                OmissionCode::PerRecordBodyCap,
                "complete body exceeded the per-record cap; selected the bounded focused span",
            );
        }
        candidates.push(Candidate {
            record_id: selected.identity.node_id.clone(),
            selection_rank: selected.selection_rank,
            selection: selected.selection.clone(),
            language: selected.symbol.language.clone(),
            slice,
            representation,
        });
    }

    let mut groups: BTreeMap<(String, String), Vec<Candidate>> = BTreeMap::new();
    for candidate in candidates {
        groups
            .entry((
                candidate.slice.span.root.clone(),
                candidate.slice.span.path.clone(),
            ))
            .or_default()
            .push(candidate);
    }
    let mut spans = Vec::new();
    for (_, mut group) in groups {
        group.sort_by(|a, b| {
            a.slice
                .span
                .start_line
                .cmp(&b.slice.span.start_line)
                .then_with(|| a.slice.span.end_line.cmp(&b.slice.span.end_line))
                .then_with(|| a.selection_rank.cmp(&b.selection_rank))
                .then_with(|| a.record_id.cmp(&b.record_id))
        });
        let mut component = Vec::new();
        let mut component_end = 0;
        for candidate in group {
            if !component.is_empty() && candidate.slice.span.start_line > component_end {
                flush_component(
                    &request,
                    &mut records,
                    &mut omissions,
                    &mut spans,
                    std::mem::take(&mut component),
                    source,
                );
                component_end = 0;
            }
            component_end = component_end.max(candidate.slice.span.end_line);
            component.push(candidate);
        }
        if !component.is_empty() {
            flush_component(
                &request,
                &mut records,
                &mut omissions,
                &mut spans,
                component,
                source,
            );
        }
    }

    spans.sort_by(span_order);
    enforce_total_body_cap(&request, &mut records, &mut omissions, &mut spans);
    annotate_projected_span_costs(&mut records, &spans);
    omissions.sort_by(omission_order);

    ProjectionPlan {
        request,
        records,
        candidate_audit: input.candidate_audit,
        spans,
        relationships: input.relationships,
        omissions,
        capabilities: input.capabilities,
    }
}

/// Bind each selected record to the deterministic body-span cost it receives.
/// The values live in evidence diagnostics, so the agent projection stays
/// concise while evidence projection can audit admission against the exact
/// coalesced span rather than an unprojected record body.
fn annotate_projected_span_costs(records: &mut [ProjectedRecord], spans: &[ProjectedSpan]) {
    for span in spans {
        let unicode_chars = span.text.chars().count();
        let estimated_tokens = unicode_chars.saturating_add(3) / 4;
        let span_id = span.source.stable_id();
        for mapping in &span.mappings {
            for record in records.iter_mut().filter(|record| {
                record.selection_rank == mapping.selection_rank
                    && record.identity.node_id == mapping.record_id
            }) {
                record
                    .evidence
                    .diagnostics
                    .insert("projected_span_id".into(), span_id.clone());
                record.evidence.diagnostics.insert(
                    "projected_span_utf8_bytes".into(),
                    span.text.len().to_string(),
                );
                record.evidence.diagnostics.insert(
                    "projected_span_unicode_chars".into(),
                    unicode_chars.to_string(),
                );
                record.evidence.diagnostics.insert(
                    "projected_span_estimated_tokens".into(),
                    estimated_tokens.to_string(),
                );
            }
        }
    }
}

fn candidate_audit_order(a: &CandidateAudit, b: &CandidateAudit) -> std::cmp::Ordering {
    a.candidate_rank
        .cmp(&b.candidate_rank)
        .then_with(|| a.identity.node_id.cmp(&b.identity.node_id))
        .then_with(|| a.identity.source.cmp(&b.identity.source))
        .then_with(|| a.disposition.cmp(&b.disposition))
        .then_with(|| a.reason.cmp(&b.reason))
        .then_with(|| evidence_order(&a.evidence, &b.evidence))
}

fn record_order(a: &SelectedRecord, b: &SelectedRecord) -> std::cmp::Ordering {
    a.selection_rank
        .cmp(&b.selection_rank)
        .then_with(|| a.identity.node_id.cmp(&b.identity.node_id))
        .then_with(|| a.selection.role.cmp(&b.selection.role))
        .then_with(|| a.selection.lane.cmp(&b.selection.lane))
        .then_with(|| a.selection.channel.cmp(&b.selection.channel))
        .then_with(|| a.selection.reason.cmp(&b.selection.reason))
        .then_with(|| a.symbol.name.cmp(&b.symbol.name))
        .then_with(|| a.symbol.kind.cmp(&b.symbol.kind))
        .then_with(|| a.symbol.language.cmp(&b.symbol.language))
        .then_with(|| a.symbol.signature.cmp(&b.symbol.signature))
        .then_with(|| a.focused_span.cmp(&b.focused_span))
        .then_with(|| evidence_order(&a.evidence, &b.evidence))
        .then_with(|| {
            a.evidence_hydration
                .as_ref()
                .map(HydrationHandle::encode)
                .cmp(&b.evidence_hydration.as_ref().map(HydrationHandle::encode))
        })
}

fn evidence_order(a: &SelectionEvidence, b: &SelectionEvidence) -> std::cmp::Ordering {
    a.raw_scores
        .cmp(&b.raw_scores)
        .then_with(|| a.content_hash.cmp(&b.content_hash))
        .then_with(|| a.candidate_rank.cmp(&b.candidate_rank))
        .then_with(|| a.provenance.cmp(&b.provenance))
        .then_with(|| a.diagnostics.cmp(&b.diagnostics))
}

fn projected_record(
    request: &ProjectionRequest,
    selected: &SelectedRecord,
    source: &SourceReader,
    omissions: &mut Vec<ProjectionOmission>,
) -> ProjectedRecord {
    let source_handle = selected.identity.source.as_ref().and_then(|full| {
        let hydrated_page = (request.intent == SearchIntent::Hydrate)
            .then_some(selected.focused_span.as_ref())
            .flatten();
        match next_hydration_span(source, full, hydrated_page) {
            Ok(Some(span)) => Some(HydrationHandle::source(
                selected.identity.node_id.clone(),
                span,
            )),
            Ok(None) => None,
            Err(error) => {
                omit(
                    omissions,
                    selected,
                    Some(full.clone()),
                    OmissionCode::SourceUnavailable,
                    &format!("source hydration handle unavailable: {error}"),
                );
                None
            }
        }
    });
    ProjectedRecord {
        selection_rank: selected.selection_rank,
        identity: selected.identity.clone(),
        symbol: selected.symbol.clone(),
        selection: selected.selection.clone(),
        evidence: selected.evidence.clone(),
        body: BodyRepresentation::SignatureOnly,
        span_ids: Vec::new(),
        source_handle,
        evidence_handle: selected.evidence_hydration.clone(),
    }
}

/// Return the next deterministic, readable page of an authoritative source
/// span. Hydration requests advance strictly past the page they just rendered,
/// so each emitted handle is consumed exactly once in source order.
fn next_hydration_span(
    source: &SourceReader,
    full: &SourceSpan,
    hydrated_page: Option<&SourceSpan>,
) -> Result<Option<SourceSpan>, SourceError> {
    let start_line = match hydrated_page {
        Some(page) if full.contains(page) && page.end_line < full.end_line => {
            page.end_line.saturating_add(1)
        }
        Some(page) if full.contains(page) => return Ok(None),
        Some(_) => return Err(SourceError::InvalidRange),
        None => full.start_line,
    };
    let remaining = SourceSpan {
        start_line,
        ..full.clone()
    };
    bounded_hydration_span(source, &remaining).map(Some)
}

/// Return the largest readable first page of the supplied remaining span.
fn bounded_hydration_span(
    source: &SourceReader,
    remaining: &SourceSpan,
) -> Result<SourceSpan, SourceError> {
    let limits = source.limits();
    let line_count = u64::from(remaining.end_line)
        .saturating_sub(u64::from(remaining.start_line))
        .saturating_add(1);
    if line_count <= u64::from(limits.max_lines) {
        match source.read(remaining) {
            Ok(_) => return Ok(remaining.clone()),
            Err(SourceError::SpanLimit(_)) => {}
            Err(error) => return Err(error),
        }
    }

    let mut low = remaining.start_line;
    let mut high = remaining
        .start_line
        .saturating_add(limits.max_lines.saturating_sub(1))
        .min(remaining.end_line);
    let mut best = None;
    while low <= high {
        let middle = low + (high - low) / 2;
        let page = SourceSpan {
            end_line: middle,
            ..remaining.clone()
        };
        match source.read(&page) {
            Ok(_) => {
                best = Some(page);
                if middle == u32::MAX {
                    break;
                }
                low = middle + 1;
            }
            Err(SourceError::SpanLimit(_)) => {
                if middle == remaining.start_line {
                    break;
                }
                high = middle - 1;
            }
            Err(error) => return Err(error),
        }
    }
    best.ok_or(SourceError::SpanLimit(limits.max_span_bytes))
}

fn flush_component(
    request: &ProjectionRequest,
    records: &mut [ProjectedRecord],
    omissions: &mut Vec<ProjectionOmission>,
    spans: &mut Vec<ProjectedSpan>,
    mut component: Vec<Candidate>,
    source: &SourceReader,
) {
    component.sort_by(|a, b| {
        a.selection_rank
            .cmp(&b.selection_rank)
            .then_with(|| a.record_id.cmp(&b.record_id))
            .then_with(|| a.selection.role.cmp(&b.selection.role))
            .then_with(|| a.selection.lane.cmp(&b.selection.lane))
            .then_with(|| a.selection.channel.cmp(&b.selection.channel))
            .then_with(|| a.selection.reason.cmp(&b.selection.reason))
            .then_with(|| a.language.cmp(&b.language))
            .then_with(|| a.slice.span.cmp(&b.slice.span))
            .then_with(|| a.slice.text.cmp(&b.slice.text))
            .then_with(|| a.representation.cmp(&b.representation))
    });
    let first = &component[0].slice.span;
    let start = component
        .iter()
        .map(|c| c.slice.span.start_line)
        .min()
        .unwrap();
    let end = component
        .iter()
        .map(|c| c.slice.span.end_line)
        .max()
        .unwrap();
    let source_span = SourceSpan {
        root: first.root.clone(),
        path: first.path.clone(),
        start_line: start,
        end_line: end,
    };
    let mut lines = BTreeMap::<u32, String>::new();
    for candidate in &component {
        for (offset, line) in exact_lines(&candidate.slice.text).into_iter().enumerate() {
            let number = candidate.slice.span.start_line + offset as u32;
            match lines.entry(number) {
                Entry::Vacant(entry) => {
                    entry.insert(line.to_string());
                }
                Entry::Occupied(entry) if entry.get() == line => {}
                Entry::Occupied(_) => {
                    for candidate in &component {
                        omissions.push(ProjectionOmission {
                            record_id: Some(candidate.record_id.clone()),
                            source: Some(candidate.slice.span.clone()),
                            code: OmissionCode::SourceUnavailable,
                            detail: "overlapping source reads disagreed".into(),
                        });
                    }
                    return;
                }
            }
        }
    }
    let mut text = String::new();
    for number in start..=end {
        let Some(line) = lines.get(&number) else {
            return;
        };
        text.push_str(line);
    }
    let mut representation = if component
        .iter()
        .any(|c| c.representation == BodyRepresentation::FocusedSpan)
    {
        BodyRepresentation::FocusedSpan
    } else {
        BodyRepresentation::Complete
    };
    if request.body_policy == BodyPolicy::Minified {
        match crate::code::minify::minify_body(&text, &component[0].language) {
            Ok(minified) => {
                text = minified.body;
                representation = BodyRepresentation::Minified;
            }
            Err(error) => {
                for candidate in &component {
                    omissions.push(ProjectionOmission {
                        record_id: Some(candidate.record_id.clone()),
                        source: Some(candidate.slice.span.clone()),
                        code: OmissionCode::MinificationFailed,
                        detail: error.to_string(),
                    });
                }
                return;
            }
        }
    }
    let mappings: Vec<_> = component
        .iter()
        .map(|candidate| SpanMapping {
            record_id: candidate.record_id.clone(),
            selection_rank: candidate.selection_rank,
            selection: candidate.selection.clone(),
            requested: candidate.slice.span.clone(),
            coverage: if representation == BodyRepresentation::Truncated {
                SpanCoverage::Partial
            } else {
                SpanCoverage::Complete
            },
        })
        .collect();
    let hydration_span = bounded_hydration_span(source, &source_span).unwrap_or_else(|_| {
        // Every component member was read successfully above, so its first
        // slice is a verified, consumable fallback even if the coalesced
        // union exceeds the reader's line or byte ceiling.
        component[0].slice.span.clone()
    });
    let span_id = source_span.stable_id();
    for mapping in &mappings {
        for record in records.iter_mut().filter(|r| {
            r.selection_rank == mapping.selection_rank && r.identity.node_id == mapping.record_id
        }) {
            record.body = representation;
            record.span_ids.push(span_id.clone());
        }
    }
    spans.push(ProjectedSpan {
        hydration: HydrationHandle::source(
            format!("span:{}", source_span.stable_id()),
            hydration_span,
        ),
        source: source_span,
        text,
        representation,
        mappings,
    });
}

fn exact_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        vec![""]
    } else {
        text.split_inclusive('\n').collect()
    }
}

fn span_order(a: &ProjectedSpan, b: &ProjectedSpan) -> std::cmp::Ordering {
    let rank = |span: &ProjectedSpan| {
        span.mappings
            .iter()
            .map(|m| m.selection_rank)
            .min()
            .unwrap_or(usize::MAX)
    };
    rank(a).cmp(&rank(b)).then_with(|| a.source.cmp(&b.source))
}

fn enforce_total_body_cap(
    request: &ProjectionRequest,
    records: &mut [ProjectedRecord],
    omissions: &mut Vec<ProjectionOmission>,
    spans: &mut Vec<ProjectedSpan>,
) {
    let Some(cap) = request.budget.total_body_bytes else {
        return;
    };
    let mut used = 0_usize;
    let mut admitted = Vec::new();
    for mut span in std::mem::take(spans) {
        let remaining = cap.saturating_sub(used);
        if span.text.len() <= remaining {
            used += span.text.len();
            admitted.push(span);
            continue;
        }
        if remaining > 0 {
            span.text = utf8_prefix(&span.text, remaining).to_string();
            used += span.text.len();
            span.representation = BodyRepresentation::Truncated;
            for mapping in &mut span.mappings {
                mapping.coverage = SpanCoverage::Partial;
            }
            update_records(records, &span, BodyRepresentation::Truncated, false);
            for mapping in &span.mappings {
                omissions.push(ProjectionOmission {
                    record_id: Some(mapping.record_id.clone()),
                    source: Some(mapping.requested.clone()),
                    code: OmissionCode::TotalBodyCap,
                    detail: format!("body truncated at the total {cap}-byte cap"),
                });
            }
            admitted.push(span);
        } else {
            update_records(records, &span, BodyRepresentation::SignatureOnly, true);
            for mapping in span.mappings {
                omissions.push(ProjectionOmission {
                    record_id: Some(mapping.record_id),
                    source: Some(mapping.requested),
                    code: OmissionCode::TotalBodyCap,
                    detail: format!("body omitted by the total {cap}-byte cap"),
                });
            }
        }
    }
    *spans = admitted;
}

fn update_records(
    records: &mut [ProjectedRecord],
    span: &ProjectedSpan,
    body: BodyRepresentation,
    remove_span: bool,
) {
    let span_id = span.source.stable_id();
    for mapping in &span.mappings {
        for record in records.iter_mut().filter(|r| {
            r.selection_rank == mapping.selection_rank && r.identity.node_id == mapping.record_id
        }) {
            record.body = body;
            if remove_span {
                record.span_ids.retain(|id| id != &span_id);
            }
        }
    }
}

pub(crate) fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn omit(
    omissions: &mut Vec<ProjectionOmission>,
    selected: &SelectedRecord,
    source: Option<SourceSpan>,
    code: OmissionCode,
    detail: &str,
) {
    omissions.push(ProjectionOmission {
        record_id: Some(selected.identity.node_id.clone()),
        source,
        code,
        detail: detail.to_string(),
    });
}

fn omission_order(a: &ProjectionOmission, b: &ProjectionOmission) -> std::cmp::Ordering {
    a.record_id
        .cmp(&b.record_id)
        .then_with(|| a.source.cmp(&b.source))
        .then_with(|| a.code.cmp(&b.code))
        .then_with(|| a.detail.cmp(&b.detail))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn record(rank: usize, id: &str, start: u32, end: u32, role: ContextRole) -> SelectedRecord {
        record_at(rank, id, "repo", "src/lib.rs", start, end, role)
    }

    #[allow(clippy::too_many_arguments)]
    fn record_at(
        rank: usize,
        id: &str,
        root: &str,
        path: &str,
        start: u32,
        end: u32,
        role: ContextRole,
    ) -> SelectedRecord {
        let source = SourceSpan {
            root: root.into(),
            path: path.into(),
            start_line: start,
            end_line: end,
        };
        SelectedRecord {
            selection_rank: rank,
            identity: RecordIdentity {
                node_id: id.into(),
                source: Some(source),
            },
            symbol: SymbolSummary {
                name: id.into(),
                kind: "function".into(),
                language: "text".into(),
                signature: format!("fn {id}()"),
                extraction_source: None,
                declared_metadata: BTreeMap::new(),
            },
            selection: SelectionSummary {
                channel: SelectionChannel::Exact,
                reason: "matched".into(),
                role: Some(role),
                lane: Some(RetrievalLane::ExactReference),
            },
            evidence: SelectionEvidence::default(),
            evidence_hydration: Some(HydrationHandle::evidence(id)),
            focused_span: None,
        }
    }

    #[test]
    fn coalesces_overlap_and_preserves_every_mapping() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "a\nb\nc\nd\n").unwrap();
        let reader = SourceReader::for_root("repo", dir.path()).unwrap();
        let mut input = ProjectionInput::default();
        input.records = vec![
            record(1, "child", 2, 3, ContextRole::EditableSource),
            record(0, "parent", 1, 3, ContextRole::DefinitionOrApiState),
        ];
        let request = ProjectionRequest {
            body_policy: BodyPolicy::Complete,
            ..Default::default()
        };
        let first = plan_projection(request.clone(), input.clone(), &reader);
        input.records.reverse();
        let second = plan_projection(request, input, &reader);
        assert_eq!(first, second);
        assert_eq!(first.spans.len(), 1);
        assert_eq!(first.spans[0].text, "a\nb\nc\n");
        assert_eq!(first.spans[0].mappings.len(), 2);
        for record in &first.records {
            assert_eq!(
                record
                    .evidence
                    .diagnostics
                    .get("projected_span_utf8_bytes")
                    .map(String::as_str),
                Some("6")
            );
            assert_eq!(
                record
                    .evidence
                    .diagnostics
                    .get("projected_span_estimated_tokens")
                    .map(String::as_str),
                Some("2")
            );
        }
    }

    #[test]
    fn candidate_audit_is_byte_ordered_independently_of_input_iteration() {
        let dir = tempfile::tempdir().unwrap();
        let reader = SourceReader::for_root("repo", dir.path()).unwrap();
        let audit = |rank: usize, id: &str, reason: &str| CandidateAudit {
            candidate_rank: rank,
            identity: RecordIdentity {
                node_id: id.into(),
                source: None,
            },
            disposition: CandidateDisposition::Omitted,
            reason: reason.into(),
            evidence: SelectionEvidence::default(),
        };
        let mut input = ProjectionInput {
            candidate_audit: vec![audit(2, "z", "later"), audit(1, "b", "same")],
            ..Default::default()
        };
        let request = ProjectionRequest::default();
        let first = plan_projection(request.clone(), input.clone(), &reader);
        input.candidate_audit.reverse();
        let second = plan_projection(request, input, &reader);

        assert_eq!(first, second);
        assert_eq!(first.candidate_audit[0].candidate_rank, 1);
        assert_eq!(first.candidate_audit[0].identity.node_id, "b");
        assert_eq!(first.candidate_audit[1].identity.node_id, "z");
    }

    #[test]
    fn total_cap_truncates_on_a_utf8_boundary() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "αβγ\n").unwrap();
        let reader = SourceReader::for_root("repo", dir.path()).unwrap();
        let input = ProjectionInput {
            records: vec![record(
                0,
                "unicode",
                1,
                1,
                ContextRole::DefinitionOrApiState,
            )],
            ..Default::default()
        };
        let request = ProjectionRequest {
            body_policy: BodyPolicy::Complete,
            budget: ProjectionBudget {
                total_body_bytes: Some(3),
                ..Default::default()
            },
            ..Default::default()
        };
        let plan = plan_projection(request, input, &reader);
        assert_eq!(plan.spans[0].text, "α");
        assert_eq!(plan.spans[0].representation, BodyRepresentation::Truncated);
    }

    #[test]
    fn partially_overlapping_and_identical_role_spans_emit_source_once() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "one\ntwo\nthree\nfour\n").unwrap();
        let reader = SourceReader::for_root("repo", dir.path()).unwrap();
        let input = ProjectionInput {
            records: vec![
                record(2, "same-test", 2, 3, ContextRole::Test),
                record(0, "left", 1, 3, ContextRole::EditableSource),
                record(1, "right", 2, 4, ContextRole::DefinitionOrApiState),
                record(3, "same-impact", 2, 3, ContextRole::CallerOrImpact),
            ],
            ..Default::default()
        };

        let plan = plan_projection(
            ProjectionRequest {
                body_policy: BodyPolicy::Complete,
                ..Default::default()
            },
            input,
            &reader,
        );

        assert_eq!(plan.spans.len(), 1);
        assert_eq!(plan.spans[0].text, "one\ntwo\nthree\nfour\n");
        assert_eq!(plan.spans[0].mappings.len(), 4);
        assert_eq!(plan.spans[0].text.matches("two\n").count(), 1);
    }

    #[test]
    fn one_identity_with_multiple_roles_updates_every_projected_record() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "body\n").unwrap();
        let reader = SourceReader::for_root("repo", dir.path()).unwrap();
        let plan = plan_projection(
            ProjectionRequest {
                body_policy: BodyPolicy::Complete,
                ..Default::default()
            },
            ProjectionInput {
                records: vec![
                    record(0, "shared", 1, 1, ContextRole::EditableSource),
                    record(0, "shared", 1, 1, ContextRole::Test),
                ],
                ..Default::default()
            },
            &reader,
        );

        assert_eq!(plan.records.len(), 2);
        assert!(
            plan.records
                .iter()
                .all(|record| record.body == BodyRepresentation::Complete)
        );
        assert_eq!(plan.spans.len(), 1);
        assert_eq!(plan.spans[0].mappings.len(), 2);
    }

    #[test]
    fn multi_root_plan_is_stable_and_root_qualified() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        fs::write(left.path().join("same.rs"), "left\n").unwrap();
        fs::write(right.path().join("same.rs"), "right\n").unwrap();
        let reader = SourceReader::new(
            [
                ("z-root".to_string(), right.path().to_path_buf()),
                ("a-root".to_string(), left.path().to_path_buf()),
            ],
            Default::default(),
        )
        .unwrap();
        let mut input = ProjectionInput {
            records: vec![
                record_at(
                    0,
                    "z",
                    "z-root",
                    "same.rs",
                    1,
                    1,
                    ContextRole::EditableSource,
                ),
                record_at(
                    0,
                    "a",
                    "a-root",
                    "same.rs",
                    1,
                    1,
                    ContextRole::EditableSource,
                ),
            ],
            ..Default::default()
        };
        let request = ProjectionRequest {
            body_policy: BodyPolicy::Complete,
            ..Default::default()
        };
        let first = plan_projection(request.clone(), input.clone(), &reader);
        input.records.reverse();
        let second = plan_projection(request, input, &reader);

        assert_eq!(first, second);
        assert_eq!(first.spans.len(), 2);
        assert_eq!(first.spans[0].source.root, "a-root");
        assert_eq!(first.spans[1].source.root, "z-root");
    }

    #[test]
    fn oversized_body_degrades_explicitly_and_keeps_hydration() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "0123456789\n").unwrap();
        let reader = SourceReader::for_root("repo", dir.path()).unwrap();
        let plan = plan_projection(
            ProjectionRequest {
                body_policy: BodyPolicy::Complete,
                budget: ProjectionBudget {
                    per_record_body_bytes: Some(4),
                    ..Default::default()
                },
                ..Default::default()
            },
            ProjectionInput {
                records: vec![record(0, "large", 1, 1, ContextRole::EditableSource)],
                ..Default::default()
            },
            &reader,
        );

        assert!(plan.spans.is_empty());
        assert_eq!(plan.records[0].body, BodyRepresentation::SignatureOnly);
        assert!(plan.records[0].source_handle.is_some());
        assert!(
            plan.omissions
                .iter()
                .any(|item| item.code == OmissionCode::PerRecordBodyCap)
        );
    }

    #[test]
    fn large_record_hydration_pages_roundtrip_authoritative_span_once_in_order() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        let text = (1..=300)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        fs::write(dir.path().join("src/lib.rs"), &text).unwrap();
        let reader = SourceReader::new(
            [("repo".to_string(), dir.path().to_path_buf())],
            super::super::source::SourceReadLimits {
                max_lines: 200,
                max_span_bytes: 512,
                max_scanned_bytes: 16 * 1024 * 1024,
            },
        )
        .unwrap();
        let authoritative = record(0, "large", 1, 300, ContextRole::EditableSource);

        let initial = plan_projection(
            ProjectionRequest::default(),
            ProjectionInput {
                records: vec![authoritative.clone()],
                ..Default::default()
            },
            &reader,
        );
        let mut next = initial.records[0].source_handle.clone();
        let mut pages = Vec::new();
        let mut hydrated = String::new();
        while let Some(handle) = next.take() {
            let handle = HydrationHandle::decode(&handle.encode()).unwrap();
            let page = handle
                .source
                .clone()
                .expect("source hydration handles contain a page");
            pages.push((page.start_line, page.end_line));

            let mut selected = authoritative.clone();
            selected.focused_span = Some(page.clone());
            let response = plan_projection(
                ProjectionRequest {
                    intent: SearchIntent::Hydrate,
                    body_policy: BodyPolicy::FocusedSpan,
                    ..Default::default()
                },
                ProjectionInput {
                    records: vec![selected],
                    ..Default::default()
                },
                &reader,
            );
            assert_eq!(response.spans.len(), 1);
            assert_eq!(response.spans[0].source, page);
            assert!(response.spans[0].text.len() <= 512);
            hydrated.push_str(&response.spans[0].text);
            next = response.records[0].source_handle.clone();
        }

        assert!(pages.len() > 2, "the byte cap must force multiple pages");
        assert_eq!(pages.first().map(|page| page.0), Some(1));
        assert_eq!(pages.last().map(|page| page.1), Some(300));
        for pair in pages.windows(2) {
            assert_eq!(pair[0].1 + 1, pair[1].0);
        }
        assert!(
            pages
                .iter()
                .all(|(start, end)| end.saturating_sub(*start) < 200)
        );
        assert_eq!(hydrated, text);
    }

    #[test]
    fn coalesced_hydration_binds_each_page_to_the_authoritative_union() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        let text = (1..=300)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        fs::write(dir.path().join("src/lib.rs"), text).unwrap();
        let reader = SourceReader::for_root("repo", dir.path()).unwrap();

        let coalesced = plan_projection(
            ProjectionRequest {
                body_policy: BodyPolicy::Complete,
                ..Default::default()
            },
            ProjectionInput {
                records: vec![
                    record(0, "first", 1, 200, ContextRole::EditableSource),
                    record(1, "second", 101, 300, ContextRole::DirectDependency),
                ],
                ..Default::default()
            },
            &reader,
        );
        assert_eq!(coalesced.spans.len(), 1);
        assert_eq!(coalesced.spans[0].source.end_line, 300);
        let coalesced_page = coalesced.spans[0]
            .hydration
            .source
            .as_ref()
            .expect("coalesced spans retain a bounded hydration page");
        assert_eq!(
            (coalesced_page.start_line, coalesced_page.end_line),
            (1, 200)
        );
        assert!(reader.read(coalesced_page).is_ok());
        assert_eq!(
            coalesced.spans[0].hydration.record_id,
            format!("span:{}", coalesced.spans[0].source.stable_id())
        );
    }

    #[test]
    fn coalesced_union_does_not_reapply_each_members_body_cap() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "aaaa\nbbbb\ncccc\n").unwrap();
        let reader = SourceReader::for_root("repo", dir.path()).unwrap();

        let plan = plan_projection(
            ProjectionRequest {
                body_policy: BodyPolicy::Complete,
                budget: ProjectionBudget {
                    per_record_body_bytes: Some(10),
                    ..Default::default()
                },
                ..Default::default()
            },
            ProjectionInput {
                records: vec![
                    record(0, "left", 1, 2, ContextRole::EditableSource),
                    record(1, "right", 2, 3, ContextRole::DirectDependency),
                ],
                ..Default::default()
            },
            &reader,
        );

        assert_eq!(plan.spans.len(), 1);
        assert_eq!(plan.spans[0].text, "aaaa\nbbbb\ncccc\n");
        assert_eq!(plan.spans[0].representation, BodyRepresentation::Complete);
        assert!(
            plan.spans[0]
                .mappings
                .iter()
                .all(|mapping| mapping.coverage == SpanCoverage::Complete)
        );
        assert!(
            plan.omissions
                .iter()
                .all(|omission| omission.code != OmissionCode::PerRecordBodyCap)
        );
    }

    #[test]
    fn oversized_complete_body_uses_and_records_the_focused_fallback() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/lib.rs"),
            "large prefix\nrequired line\nlarge suffix\n",
        )
        .unwrap();
        let reader = SourceReader::for_root("repo", dir.path()).unwrap();
        let mut selected = record(0, "large", 1, 3, ContextRole::EditableSource);
        selected.focused_span = Some(SourceSpan {
            root: "repo".into(),
            path: "src/lib.rs".into(),
            start_line: 2,
            end_line: 2,
        });
        let plan = plan_projection(
            ProjectionRequest {
                body_policy: BodyPolicy::Complete,
                budget: ProjectionBudget {
                    per_record_body_bytes: Some(20),
                    ..Default::default()
                },
                ..Default::default()
            },
            ProjectionInput {
                records: vec![selected],
                ..Default::default()
            },
            &reader,
        );

        assert_eq!(plan.spans[0].text, "required line\n");
        assert_eq!(
            plan.spans[0].representation,
            BodyRepresentation::FocusedSpan
        );
        assert_eq!(plan.records[0].body, BodyRepresentation::FocusedSpan);
        assert!(plan.omissions.iter().any(|item| {
            item.code == OmissionCode::PerRecordBodyCap && item.detail.contains("focused span")
        }));
    }

    #[test]
    fn signature_and_no_body_policies_are_explicit_and_hydratable() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "body\n").unwrap();
        let reader = SourceReader::for_root("repo", dir.path()).unwrap();
        let input = ProjectionInput {
            records: vec![record(0, "bounded", 1, 1, ContextRole::EditableSource)],
            ..Default::default()
        };

        let signature = plan_projection(
            ProjectionRequest {
                body_policy: BodyPolicy::SignatureOnly,
                ..Default::default()
            },
            input.clone(),
            &reader,
        );
        let none = plan_projection(
            ProjectionRequest {
                body_policy: BodyPolicy::NoBody,
                ..Default::default()
            },
            input,
            &reader,
        );

        assert!(signature.spans.is_empty());
        assert_eq!(signature.records[0].body, BodyRepresentation::SignatureOnly);
        assert!(signature.records[0].source_handle.is_some());
        assert!(none.spans.is_empty());
        assert_eq!(none.records[0].body, BodyRepresentation::SignatureOnly);
        assert!(none.records[0].source_handle.is_some());
        assert!(
            none.omissions
                .iter()
                .any(|item| item.code == OmissionCode::NoBodyPolicy)
        );
    }
}
