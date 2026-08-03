//! Canonical agent/evidence rendering with self-inclusive cost accounting.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use super::model::*;
use super::projection::utf8_prefix;

const ESTIMATE_NAME: &str = "unicode_chars_div_4_ceiling";
const TASK_OBLIGATION_EXCERPT_BYTES: usize = 1_024;
const TASK_OBLIGATION_CONTEXT_LINES: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RenderError {
    BudgetTooSmall { minimum: RenderCost },
    AccountingDidNotConverge,
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BudgetTooSmall { minimum } => write!(
                f,
                "render budget is smaller than the non-body response ({} bytes, {} estimated tokens)",
                minimum.utf8_bytes, minimum.estimated_tokens
            ),
            Self::AccountingDidNotConverge => {
                f.write_str("self-inclusive render accounting did not converge")
            }
        }
    }
}

impl std::error::Error for RenderError {}

pub(crate) fn render_projection(plan: &ProjectionPlan) -> Result<RenderedResponse, RenderError> {
    let mut plan = plan.clone();
    // Every degradation step below is monotonic.  The generous bound is only
    // a guard against programming errors; it is not part of admission policy.
    let attempts = plan
        .spans
        .len()
        .saturating_mul(3)
        .saturating_add(plan.records.len().saturating_mul(5))
        .saturating_add(plan.relationships.len().saturating_mul(2))
        .saturating_add(32);
    for _ in 0..attempts {
        let (text, accounting) = render_once(&plan)?;
        if within_budget(&accounting.total, &plan.request.budget) {
            return Ok(RenderedResponse {
                text,
                accounting,
                plan,
            });
        }
        if !compact_capability_diagnostics(&mut plan)
            && !compact_candidate_audit(&mut plan)
            && !compact_capability_list(&mut plan)
            // Omission rows describe evidence that is not in the packet. In
            // task mode they must yield before audit detail or source bodies
            // from the actionable evidence that is actually being delivered.
            && !compact_omissions(&mut plan)
            && !shrink_last_record_evidence(&mut plan)
            && !compact_record_metadata(&mut plan)
            && !compact_relationship_details(&mut plan)
            && !shrink_last_relationship(&mut plan)
            && !compact_last_task_body_to_obligation_excerpt(&mut plan)
            && !shrink_last_body(&mut plan, &accounting.total)
            && !drop_last_flat_record(&mut plan)
        {
            return Err(RenderError::BudgetTooSmall {
                minimum: accounting.total,
            });
        }
    }
    Err(RenderError::AccountingDidNotConverge)
}

fn compact_capability_diagnostics(plan: &mut ProjectionPlan) -> bool {
    if plan
        .capabilities
        .iter()
        .any(|capability| !capability.detail.is_empty())
    {
        for capability in &mut plan.capabilities {
            capability.detail.clear();
        }
        add_compact_degradation(
            plan,
            "capability diagnostics omitted; hydrate or retry with a larger budget",
        );
        return true;
    }
    false
}

fn compact_capability_list(plan: &mut ProjectionPlan) -> bool {
    // Graph-delta capability names are part of that projection's delivery
    // contract, not diagnostics. Preserve their states after details and
    // duplicate candidate audit have been compacted.
    if plan
        .records
        .iter()
        .any(|record| record.selection.role == Some(ContextRole::ProposalDelta))
    {
        return false;
    }
    if plan.capabilities.len() <= 1 {
        return false;
    }
    let degraded = plan
        .capabilities
        .iter()
        .filter(|item| item.state != CapabilityState::Ready)
        .count();
    let total = plan.capabilities.len();
    plan.capabilities = vec![CapabilityStatus {
        capability: "delivery_capabilities".into(),
        state: if degraded == 0 {
            CapabilityState::Ready
        } else {
            CapabilityState::Degraded
        },
        detail: String::new(),
    }];
    add_compact_degradation(
        plan,
        &format!("capability list compacted; total={total} degraded={degraded}"),
    );
    true
}

fn compact_candidate_audit(plan: &mut ProjectionPlan) -> bool {
    if plan.request.projection != SearchProjection::Evidence || plan.candidate_audit.is_empty() {
        return false;
    }
    let count = plan.candidate_audit.len();
    plan.candidate_audit.clear();
    add_compact_degradation(
        plan,
        &format!(
            "candidate audit omitted; count={count}; selected identities and hydration handles retained"
        ),
    );
    true
}

fn compact_record_metadata(plan: &mut ProjectionPlan) -> bool {
    let task_intent = plan.request.intent == SearchIntent::Implement;
    let Some(index) = plan.records.iter().rposition(|record| {
        !record.symbol.declared_metadata.is_empty()
            || record.symbol.extraction_source.is_some()
            || (record.selection.reason.len() > 32
                && !(task_intent
                    && compact_task_reason(&record.selection.reason)))
    }) else {
        return false;
    };
    let compact_reason = task_intent.then(|| compact_task_selection_reason(plan, index));
    let record = &mut plan.records[index];
    record.symbol.declared_metadata.clear();
    record.symbol.extraction_source = None;
    if record.selection.reason.len() > 32 {
        record.selection.reason = if task_intent {
            compact_reason.expect("task reason was computed before the mutable borrow")
        } else {
            "selected; hydrate for detail".into()
        };
    }
    true
}

fn compact_task_reason(reason: &str) -> bool {
    reason.starts_with("quality=")
        && (reason.contains("; obligations_visible=") || reason.contains("; obligations="))
}

fn compact_task_selection_reason(plan: &ProjectionPlan, record_index: usize) -> String {
    let record = &plan.records[record_index];
    let reason = &record.selection.reason;
    let quality = reason
        .find("quality=")
        .map(|start| {
            reason[start..]
                .split(';')
                .next()
                .unwrap_or("quality=actionable")
        })
        .unwrap_or("quality=actionable");
    let obligations = task_obligations_from_reason(reason);
    let visible_text = visible_record_text(plan, record);
    let (visible, hydrate): (Vec<_>, Vec<_>) = obligations
        .into_iter()
        .partition(|obligation| obligation_is_visible(obligation, record, &visible_text));
    format!(
        "{quality}; obligations_visible={}; obligations_hydrate={}",
        obligation_list(&visible),
        obligation_list(&hydrate)
    )
}

fn obligation_list(obligations: &[String]) -> String {
    if obligations.is_empty() {
        "none".into()
    } else {
        obligations.join(",")
    }
}

fn task_obligations_from_reason(reason: &str) -> BTreeSet<String> {
    let mut obligations = BTreeSet::new();
    for marker in [
        "obligations=",
        "obligations_visible=",
        "obligations_hydrate=",
    ] {
        let Some(start) = reason.find(marker) else {
            continue;
        };
        let value = &reason[start + marker.len()..];
        let value = value.split(';').next().unwrap_or(value);
        obligations.extend(
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty() && *value != "none")
                .filter(|value| !value.starts_with("branch:"))
                .map(str::to_string),
        );
    }
    obligations
}

fn visible_record_text(plan: &ProjectionPlan, record: &ProjectedRecord) -> String {
    let mut text = record.symbol.signature.clone();
    for span in &plan.spans {
        if span.mappings.iter().any(|mapping| {
            mapping.selection_rank == record.selection_rank
                && mapping.record_id == record.identity.node_id
        }) {
            text.push('\n');
            text.push_str(&span.text);
        }
    }
    text
}

fn obligation_is_visible(
    obligation: &str,
    record: &ProjectedRecord,
    visible_text: &str,
) -> bool {
    if obligation == "validation:task-relevant-tests" {
        return record.selection.role == Some(ContextRole::Test)
            && matches!(record.symbol.kind.as_str(), "function" | "method");
    }
    let terms = obligation_terms(obligation);
    !terms.is_empty()
        && terms
            .iter()
            .all(|term| text_contains_identifier_term(visible_text, term))
}

fn obligation_terms(obligation: &str) -> Vec<&str> {
    if let Some(term) = obligation.strip_prefix("concept:") {
        return vec![term];
    }
    if let Some(terms) = obligation.strip_prefix("proof:") {
        return terms.split('+').filter(|term| !term.is_empty()).collect();
    }
    if let Some(terms) = obligation.strip_prefix("state:") {
        return terms.split('+').filter(|term| !term.is_empty()).collect();
    }
    obligation
        .strip_prefix("structure:")
        .and_then(|rest| rest.rsplit_once(':').map(|(_, terms)| terms))
        .map(|terms| terms.split('+').filter(|term| !term.is_empty()).collect())
        .or_else(|| {
            obligation
                .strip_prefix("carrier:")
                .and_then(|rest| rest.rsplit_once(':').map(|(_, carrier)| vec![carrier]))
        })
        .unwrap_or_default()
}

fn text_contains_identifier_term(text: &str, term: &str) -> bool {
    let term = term.to_lowercase();
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|chunk| !chunk.is_empty())
        .any(|chunk| {
            let lower = chunk.to_lowercase();
            lower == term || camel_segments(chunk).iter().any(|segment| segment == &term)
        })
}

fn camel_segments(value: &str) -> Vec<String> {
    let characters = value.chars().collect::<Vec<_>>();
    let mut segments = Vec::new();
    let mut start = 0;
    for index in 1..characters.len() {
        let boundary = characters[index].is_uppercase()
            && (characters[index - 1].is_lowercase()
                || characters
                    .get(index + 1)
                    .is_some_and(|next| next.is_lowercase()));
        if boundary {
            segments.push(characters[start..index].iter().collect::<String>().to_lowercase());
            start = index;
        }
    }
    if start < characters.len() {
        segments.push(characters[start..].iter().collect::<String>().to_lowercase());
    }
    segments
}

fn compact_relationship_details(plan: &mut ProjectionPlan) -> bool {
    let Some(relationship) = plan
        .relationships
        .iter_mut()
        .rev()
        .find(|edge| !edge.reason.is_empty())
    else {
        return false;
    };
    relationship.reason.clear();
    true
}

fn shrink_last_relationship(plan: &mut ProjectionPlan) -> bool {
    if plan.relationships.is_empty() {
        return false;
    }
    plan.relationships.pop();
    add_compact_degradation(
        plan,
        "lower-value relationship detail omitted; retained records remain hydratable",
    );
    true
}

fn drop_last_flat_record(plan: &mut ProjectionPlan) -> bool {
    // Task packets protect selected evidence units: selection coverage is only
    // truthful if every selected task identity survives fitting. Flat search
    // has no such obligation certificate and may shed its lowest-ranked tail.
    if plan.request.intent == SearchIntent::Implement || plan.records.len() <= 1 {
        return false;
    }
    let removed = plan.records.pop().expect("non-empty record list");
    let id = removed.identity.node_id.clone();
    let source = removed.identity.source.clone();
    let hydration = removed
        .source_handle
        .as_ref()
        .or(removed.evidence_handle.as_ref())
        .map(HydrationHandle::encode_compact);
    for span in &mut plan.spans {
        span.mappings.retain(|mapping| mapping.record_id != id);
    }
    plan.spans.retain(|span| !span.mappings.is_empty());
    plan.omissions.push(ProjectionOmission {
        record_id: Some(id),
        source,
        code: OmissionCode::RenderBudget,
        detail: hydration.map_or_else(
            || "lower-ranked record omitted; no stable hydration handle was available".into(),
            |handle| format!("lower-ranked record omitted; hydrate={handle}"),
        ),
    });
    true
}

fn compact_omissions(plan: &mut ProjectionPlan) -> bool {
    if plan.omissions.len() <= 1 {
        return false;
    }
    let count = plan.omissions.len();
    let retained_hydration = plan
        .omissions
        .iter()
        .find(|omission| omission.detail.contains("hydrate=rna-h2:"))
        .cloned();
    plan.omissions = vec![retained_hydration.map_or_else(
        || ProjectionOmission {
            record_id: None,
            source: None,
            code: OmissionCode::RenderBudget,
            detail: format!("delivery metadata compacted; omitted_detail_count={count}"),
        },
        |mut omission| {
            omission.detail = format!("{}; omitted_detail_count={count}", omission.detail);
            omission
        },
    )];
    true
}

fn add_compact_degradation(plan: &mut ProjectionPlan, detail: &str) {
    if plan
        .omissions
        .iter()
        .any(|item| item.record_id.is_none() && item.source.is_none() && item.detail == detail)
    {
        return;
    }
    plan.omissions.push(ProjectionOmission {
        record_id: None,
        source: None,
        code: OmissionCode::RenderBudget,
        detail: detail.to_string(),
    });
}

fn shrink_last_record_evidence(plan: &mut ProjectionPlan) -> bool {
    if plan.request.projection != SearchProjection::Evidence {
        return false;
    }
    let Some(record) = plan.records.iter_mut().rev().find(|record| {
        record.evidence != SelectionEvidence::default() && record.evidence_handle.is_some()
    }) else {
        return false;
    };
    let record_id = record.identity.node_id.clone();
    record.evidence = SelectionEvidence::default();
    let detail = "selected record audit detail omitted to satisfy the final rendered budget; hydrate evidence for the complete audit";
    if !plan.omissions.iter().any(|omission| {
        omission.record_id.as_deref() == Some(record_id.as_str())
            && omission.code == OmissionCode::RenderBudget
            && omission.detail == detail
    }) {
        plan.omissions.push(ProjectionOmission {
            record_id: Some(record_id),
            source: None,
            code: OmissionCode::RenderBudget,
            detail: detail.to_string(),
        });
        plan.omissions.sort_by(|a, b| {
            a.record_id
                .cmp(&b.record_id)
                .then_with(|| a.source.cmp(&b.source))
                .then_with(|| a.code.cmp(&b.code))
                .then_with(|| a.detail.cmp(&b.detail))
        });
    }
    true
}

fn within_budget(cost: &RenderCost, budget: &ProjectionBudget) -> bool {
    budget
        .max_rendered_bytes
        .is_none_or(|limit| cost.utf8_bytes <= limit)
        && budget
            .max_estimated_tokens
            .is_none_or(|limit| cost.estimated_tokens <= limit)
}

fn compact_last_task_body_to_obligation_excerpt(plan: &mut ProjectionPlan) -> bool {
    if plan.request.intent != SearchIntent::Implement {
        return false;
    }
    let Some((index, excerpt)) = plan
        .spans
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, span)| {
            !span.text.is_empty() && span.representation != BodyRepresentation::Truncated
        })
        .find_map(|(index, span)| obligation_excerpt(span).map(|excerpt| (index, excerpt)))
    else {
        return false;
    };

    let mut affected = Vec::new();
    {
        let span = &mut plan.spans[index];
        span.text = excerpt;
        span.representation = BodyRepresentation::Truncated;
        for mapping in &mut span.mappings {
            mapping.coverage = SpanCoverage::Partial;
            affected.push(mapping.clone());
        }
    }
    let snapshot = plan.spans[index].clone();
    update_records(plan, &snapshot, BodyRepresentation::Truncated, false);
    for mapping in affected {
        add_budget_omission(
            plan,
            &mapping,
            "body reduced to a bounded obligation-carrier excerpt; hydrate source for complete context",
        );
    }
    refresh_compact_task_obligation_visibility(plan);
    true
}

fn obligation_excerpt(span: &ProjectedSpan) -> Option<String> {
    let terms = span
        .mappings
        .iter()
        .flat_map(|mapping| task_obligations_from_reason(&mapping.selection.reason))
        .flat_map(|obligation| {
            obligation_terms(&obligation)
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect::<BTreeSet<_>>();
    if terms.is_empty() {
        return None;
    }

    let lines = span.text.lines().collect::<Vec<_>>();
    let mut retained = BTreeSet::new();
    for (index, line) in lines.iter().enumerate() {
        if terms
            .iter()
            .any(|term| text_contains_identifier_term(line, term))
        {
            let start = index.saturating_sub(TASK_OBLIGATION_CONTEXT_LINES);
            let end = (index + TASK_OBLIGATION_CONTEXT_LINES + 1).min(lines.len());
            retained.extend(start..end);
        }
    }
    if retained.is_empty() {
        return None;
    }

    let mut excerpt = String::new();
    let mut previous = None;
    for index in retained {
        if previous.is_some_and(|previous| index > previous + 1)
            && !push_excerpt_piece(&mut excerpt, "…\n")
        {
            break;
        }
        let mut line = lines[index].to_string();
        line.push('\n');
        if !push_excerpt_piece(&mut excerpt, &line) {
            break;
        }
        previous = Some(index);
    }
    if excerpt.is_empty() || excerpt.len() >= span.text.len() {
        None
    } else {
        Some(excerpt)
    }
}

fn push_excerpt_piece(excerpt: &mut String, piece: &str) -> bool {
    if excerpt.len().saturating_add(piece.len()) > TASK_OBLIGATION_EXCERPT_BYTES {
        return false;
    }
    excerpt.push_str(piece);
    true
}

fn refresh_compact_task_obligation_visibility(plan: &mut ProjectionPlan) {
    let updates = plan
        .records
        .iter()
        .enumerate()
        .filter(|(_, record)| compact_task_reason(&record.selection.reason))
        .map(|(index, _)| (index, compact_task_selection_reason(plan, index)))
        .collect::<Vec<_>>();
    for (index, reason) in updates {
        plan.records[index].selection.reason = reason;
    }
}

fn shrink_last_body(plan: &mut ProjectionPlan, cost: &RenderCost) -> bool {
    let Some(index) = plan.spans.iter().rposition(|span| {
        !span.text.is_empty() && !span_requires_visible_proof_body(plan, span)
    }) else {
        return false;
    };
    let span = &plan.spans[index];
    let byte_reduction = plan
        .request
        .budget
        .max_rendered_bytes
        .map_or(0, |limit| cost.utf8_bytes.saturating_sub(limit));
    let allowed_chars = plan
        .request
        .budget
        .max_estimated_tokens
        .map(|limit| limit.saturating_mul(4));
    let char_reduction = allowed_chars.map_or(0, |limit| cost.unicode_chars.saturating_sub(limit));
    let target_bytes = span.text.len().saturating_sub(byte_reduction.max(1));
    let target_chars = span.text.chars().count().saturating_sub(char_reduction);
    let by_bytes = utf8_prefix(&span.text, target_bytes);
    let prefix: String = by_bytes.chars().take(target_chars).collect();

    if prefix.is_empty() {
        let removed = plan.spans.remove(index);
        update_records(plan, &removed, BodyRepresentation::SignatureOnly, true);
        for mapping in removed.mappings {
            add_budget_omission(
                plan,
                &mapping,
                "body omitted to satisfy the final rendered budget",
            );
        }
    } else {
        let mut affected = Vec::new();
        {
            let span = &mut plan.spans[index];
            span.text = prefix;
            span.representation = BodyRepresentation::Truncated;
            for mapping in &mut span.mappings {
                mapping.coverage = SpanCoverage::Partial;
                affected.push(mapping.clone());
            }
        }
        let snapshot = plan.spans[index].clone();
        update_records(plan, &snapshot, BodyRepresentation::Truncated, false);
        for mapping in affected {
            add_budget_omission(
                plan,
                &mapping,
                "body truncated to satisfy the final rendered budget",
            );
        }
    }
    refresh_compact_task_obligation_visibility(plan);
    true
}

fn span_requires_visible_proof_body(plan: &ProjectionPlan, span: &ProjectedSpan) -> bool {
    span.mappings.iter().any(|mapping| {
        let Some(record) = plan.records.iter().find(|record| {
            record.selection_rank == mapping.selection_rank
                && record.identity.node_id == mapping.record_id
        }) else {
            return false;
        };
        task_obligations_from_reason(&record.selection.reason)
            .into_iter()
            .any(|obligation| obligation.starts_with("proof:"))
    })
}

fn update_records(
    plan: &mut ProjectionPlan,
    span: &ProjectedSpan,
    body: BodyRepresentation,
    remove_span: bool,
) {
    let span_id = span.source.stable_id();
    for mapping in &span.mappings {
        for record in plan.records.iter_mut().filter(|record| {
            record.selection_rank == mapping.selection_rank
                && record.identity.node_id == mapping.record_id
        }) {
            record.body = body;
            if remove_span {
                record.span_ids.retain(|id| id != &span_id);
            }
        }
    }
}

fn add_budget_omission(plan: &mut ProjectionPlan, mapping: &SpanMapping, detail: &str) {
    let omission = ProjectionOmission {
        record_id: Some(mapping.record_id.clone()),
        source: Some(mapping.requested.clone()),
        code: OmissionCode::RenderBudget,
        detail: detail.to_string(),
    };
    if !plan.omissions.contains(&omission) {
        plan.omissions.push(omission);
    }
    plan.omissions.sort_by(|a, b| {
        a.record_id
            .cmp(&b.record_id)
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| a.code.cmp(&b.code))
            .then_with(|| a.detail.cmp(&b.detail))
    });
}

fn render_once(plan: &ProjectionPlan) -> Result<(String, RenderAccounting), RenderError> {
    let fixed = [
        (CostSection::Headers, render_headers(plan)),
        (CostSection::Metadata, render_metadata(plan)),
        (CostSection::Bodies, render_bodies(plan)),
        (CostSection::Relationships, render_relationships(plan)),
    ];
    let mut footer = String::new();
    for _ in 0..64 {
        let accounting = accounting(&fixed, &footer);
        let next = render_footer(&accounting);
        if next == footer {
            let mut text = String::new();
            for (_, section) in &fixed {
                text.push_str(section);
            }
            text.push_str(&footer);
            debug_assert_eq!(text.len(), accounting.total.utf8_bytes);
            debug_assert_eq!(text.chars().count(), accounting.total.unicode_chars);
            return Ok((text, accounting));
        }
        footer = next;
    }
    Err(RenderError::AccountingDidNotConverge)
}

fn accounting(fixed: &[(CostSection, String)], footer: &str) -> RenderAccounting {
    let mut sections = BTreeMap::new();
    for (kind, text) in fixed {
        sections.insert(*kind, measure(text));
    }
    sections.insert(CostSection::Footer, measure(footer));
    let utf8_bytes = sections.values().map(|cost| cost.utf8_bytes).sum();
    let unicode_chars = sections.values().map(|cost| cost.unicode_chars).sum();
    RenderAccounting {
        total: RenderCost {
            utf8_bytes,
            unicode_chars,
            estimated_tokens: estimate_tokens(unicode_chars),
        },
        sections,
        estimate_name: ESTIMATE_NAME.to_string(),
        provider_usage: false,
    }
}

fn measure(value: &str) -> RenderCost {
    let unicode_chars = value.chars().count();
    RenderCost {
        utf8_bytes: value.len(),
        unicode_chars,
        estimated_tokens: estimate_tokens(unicode_chars),
    }
}

fn estimate_tokens(chars: usize) -> usize {
    chars.saturating_add(3) / 4
}

fn render_headers(plan: &ProjectionPlan) -> String {
    format!(
        "# RNA search context\n\n- projection: {}\n- intent: {}\n- body_policy: {}\n- selected_records: {}\n",
        plan.request.projection,
        plan.request.intent,
        plan.request.body_policy,
        plan.records.len()
    )
}

fn render_metadata(plan: &ProjectionPlan) -> String {
    let mut output = String::new();
    if !plan.capabilities.is_empty() {
        output.push_str("\n## Capability status\n");
        for capability in &plan.capabilities {
            output.push_str(&format!(
                "\n- {}: {}",
                one_line(&capability.capability),
                capability.state
            ));
            if (plan.request.projection == SearchProjection::Evidence
                || capability.capability == "readiness_diagnostics")
                && !capability.detail.is_empty()
            {
                output.push_str(&format!(" — {}", one_line(&capability.detail)));
            }
        }
        output.push('\n');
    }
    output.push_str("\n## Results\n");
    if plan.records.is_empty() {
        output.push_str("\nNo selected records.\n");
    }
    for record in &plan.records {
        let location = record.identity.source.as_ref().map_or_else(
            || "no source".to_string(),
            |span| {
                format!(
                    "[{}] {}:{}-{}",
                    span.root, span.path, span.start_line, span.end_line
                )
            },
        );
        output.push_str(&format!(
            "\n{}. {} `{}` — {}\n   - id: {}\n   - signature: {}\n   - channel: {}; reason: {}\n   - body: {}\n",
            record.selection_rank + 1,
            one_line(&record.symbol.kind),
            one_line(&record.symbol.name),
            location,
            inline_code(&record.identity.node_id),
            inline_code(&one_line(&record.symbol.signature)),
            record.selection.channel,
            one_line(&record.selection.reason),
            record.body,
        ));
        if let Some(role) = record.selection.role {
            output.push_str(&format!("   - role: {role}\n"));
        }
        if let Some(lane) = record.selection.lane {
            output.push_str(&format!("   - lane: {lane}\n"));
        }
        if let Some(source) = &record.symbol.extraction_source {
            output.push_str(&format!(
                "   - extraction_source: src:{}\n",
                one_line(source)
            ));
        }
        for (name, value) in &record.symbol.declared_metadata {
            output.push_str(&format!(
                "   - metadata.{}: {}\n",
                one_line(name),
                one_line(value)
            ));
        }
        if let Some(handle) = &record.source_handle {
            output.push_str(&format!(
                "   - hydrate_source: {}\n",
                inline_code(&handle.encode_compact())
            ));
        }
        if let Some(handle) = &record.evidence_handle {
            output.push_str(&format!(
                "   - hydrate_evidence: {}\n",
                inline_code(&handle.encode_compact())
            ));
        }
        if plan.request.projection == SearchProjection::Evidence {
            render_evidence(&mut output, &record.evidence);
        }
    }
    if plan.request.projection == SearchProjection::Evidence && !plan.candidate_audit.is_empty() {
        output.push_str("\n## Candidate audit\n");
        for candidate in &plan.candidate_audit {
            let location = candidate.identity.source.as_ref().map_or_else(
                || "no source".to_string(),
                |span| {
                    format!(
                        "[{}] {}:{}-{}",
                        span.root, span.path, span.start_line, span.end_line
                    )
                },
            );
            output.push_str(&format!(
                "\n{}. {} — {}; disposition={}; reason={}\n",
                candidate.candidate_rank,
                inline_code(&candidate.identity.node_id),
                location,
                candidate.disposition,
                one_line(&candidate.reason),
            ));
            render_evidence(&mut output, &candidate.evidence);
        }
    }
    if !plan.omissions.is_empty() {
        output.push_str("\n## Omissions and degradation\n");
        for omission in &plan.omissions {
            output.push_str(&format!(
                "\n- {}: {}",
                omission.code,
                one_line(&omission.detail)
            ));
            if let Some(record) = &omission.record_id {
                output.push_str(&format!("; record={}", inline_code(record)));
            }
        }
        output.push('\n');
    }
    output
}

fn render_evidence(output: &mut String, evidence: &SelectionEvidence) {
    if let Some(rank) = evidence.candidate_rank {
        output.push_str(&format!("   - evidence.candidate_rank: {rank}\n"));
    }
    if let Some(hash) = &evidence.content_hash {
        output.push_str(&format!(
            "   - evidence.content_hash: {}\n",
            inline_code(hash)
        ));
    }
    for (name, score) in &evidence.raw_scores {
        output.push_str(&format!(
            "   - evidence.score.{}: {}\n",
            one_line(name),
            one_line(score)
        ));
    }
    let mut provenance = evidence.provenance.clone();
    provenance.sort();
    for item in provenance {
        output.push_str(&format!(
            "   - evidence.provenance: {} — {}\n",
            one_line(&item.source),
            one_line(&item.detail)
        ));
    }
    for (name, diagnostic) in &evidence.diagnostics {
        output.push_str(&format!(
            "   - evidence.diagnostic.{}: {}\n",
            one_line(name),
            one_line(diagnostic)
        ));
    }
}

fn render_bodies(plan: &ProjectionPlan) -> String {
    if plan.spans.is_empty() {
        return String::new();
    }
    let mut output = "\n## Source bodies\n".to_string();
    for span in &plan.spans {
        output.push_str(&format!(
            "\n### [{}] `{}`:{}-{} ({})\n",
            span.source.root,
            span.source.path,
            span.source.start_line,
            span.source.end_line,
            span.representation
        ));
        output.push_str(&format!(
            "- hydrate: {}\n",
            inline_code(&span.hydration.encode_compact())
        ));
        for mapping in &span.mappings {
            output.push_str(&format!(
                "- satisfies: {} role={} channel={} coverage={} reason={}\n",
                inline_code(&mapping.record_id),
                mapping
                    .selection
                    .role
                    .map_or("unknown", ContextRole::as_str),
                mapping.selection.channel,
                mapping.coverage,
                one_line(&mapping.selection.reason)
            ));
        }
        let fence = safe_fence(&span.text);
        output.push_str(&format!("\n{fence}text\n{}{fence}\n", span.text));
    }
    output
}

fn render_relationships(plan: &ProjectionPlan) -> String {
    if plan.relationships.is_empty() {
        return String::new();
    }
    let mut output = "\n## Relationships\n".to_string();
    for relationship in &plan.relationships {
        output.push_str(&format!(
            "\n- {} --{}--> {}: {}",
            inline_code(&relationship.from),
            one_line(&relationship.kind),
            inline_code(&relationship.to),
            one_line(&relationship.reason)
        ));
    }
    output.push('\n');
    output
}

fn render_footer(accounting: &RenderAccounting) -> String {
    let section = |kind| accounting.sections.get(&kind).copied().unwrap_or_default();
    let mut output = format!(
        "\n## Render accounting\n\n- total: bytes={} chars={} estimated_tokens={}\n- estimate: {} (deterministic estimate; not provider usage)\n- sections:\n",
        accounting.total.utf8_bytes,
        accounting.total.unicode_chars,
        accounting.total.estimated_tokens,
        accounting.estimate_name
    );
    for kind in [
        CostSection::Headers,
        CostSection::Bodies,
        CostSection::Relationships,
        CostSection::Metadata,
        CostSection::Footer,
    ] {
        let cost = section(kind);
        output.push_str(&format!(
            "  - {kind}: bytes={} chars={} estimated_tokens={}\n",
            cost.utf8_bytes, cost.unicode_chars, cost.estimated_tokens
        ));
    }
    output
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn inline_code(value: &str) -> String {
    let longest = value
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(longest.saturating_add(1).max(1));
    format!("{fence} {value} {fence}")
}

fn safe_fence(value: &str) -> String {
    let longest = value
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    "`".repeat(longest.saturating_add(1).max(3))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn plan(projection: SearchProjection) -> ProjectionPlan {
        let source = SourceSpan {
            root: "repo".into(),
            path: "src/ü.rs".into(),
            start_line: 1,
            end_line: 1,
        };
        let selection = SelectionSummary {
            channel: SelectionChannel::Semantic,
            reason: "useful".into(),
            role: Some(ContextRole::DefinitionOrApiState),
            lane: Some(RetrievalLane::DefinitionOrState),
        };
        let mut evidence = SelectionEvidence::default();
        evidence.raw_scores.insert("cosine".into(), "0.75".into());
        evidence.content_hash = Some("secret-hash".into());
        ProjectionPlan {
            request: ProjectionRequest {
                projection,
                body_policy: BodyPolicy::Complete,
                ..Default::default()
            },
            records: vec![ProjectedRecord {
                selection_rank: 0,
                identity: RecordIdentity {
                    node_id: "node".into(),
                    source: Some(source.clone()),
                },
                symbol: SymbolSummary {
                    name: "β".into(),
                    kind: "function".into(),
                    language: "rust".into(),
                    signature: "fn β()".into(),
                    extraction_source: None,
                    declared_metadata: BTreeMap::new(),
                },
                selection: selection.clone(),
                evidence,
                body: BodyRepresentation::Complete,
                span_ids: vec![source.stable_id()],
                source_handle: Some(HydrationHandle::source("node", source.clone())),
                evidence_handle: Some(HydrationHandle::evidence("node")),
            }],
            candidate_audit: vec![CandidateAudit {
                candidate_rank: 2,
                identity: RecordIdentity {
                    node_id: "omitted-node".into(),
                    source: None,
                },
                disposition: CandidateDisposition::Omitted,
                reason: "outside bounded selection".into(),
                evidence: SelectionEvidence {
                    content_hash: Some("audit-secret-hash".into()),
                    ..Default::default()
                },
            }],
            spans: vec![ProjectedSpan {
                source: source.clone(),
                text: "β();\n".into(),
                representation: BodyRepresentation::Complete,
                mappings: vec![SpanMapping {
                    record_id: "node".into(),
                    selection_rank: 0,
                    selection,
                    requested: source.clone(),
                    coverage: SpanCoverage::Complete,
                }],
                hydration: HydrationHandle::source("node", source),
            }],
            relationships: vec![],
            omissions: vec![],
            capabilities: vec![CapabilityStatus {
                capability: "lsp".into(),
                state: CapabilityState::Unavailable,
                detail: "internal-scorer-path=/tmp/private-model".into(),
            }],
        }
    }

    fn empty_plan(projection: SearchProjection) -> ProjectionPlan {
        ProjectionPlan {
            request: ProjectionRequest {
                projection,
                ..Default::default()
            },
            records: Vec::new(),
            candidate_audit: Vec::new(),
            spans: Vec::new(),
            relationships: Vec::new(),
            omissions: Vec::new(),
            capabilities: Vec::new(),
        }
    }

    fn unicode_signature_plan() -> ProjectionPlan {
        ProjectionPlan {
            request: ProjectionRequest::default(),
            records: vec![ProjectedRecord {
                selection_rank: 0,
                identity: RecordIdentity {
                    node_id: "node:β".into(),
                    source: Some(SourceSpan {
                        root: "répo".into(),
                        path: "src/ü.rs".into(),
                        start_line: 1,
                        end_line: 1,
                    }),
                },
                symbol: SymbolSummary {
                    name: "β".into(),
                    kind: "function".into(),
                    language: "rust".into(),
                    signature: "fn β()".into(),
                    extraction_source: None,
                    declared_metadata: BTreeMap::new(),
                },
                selection: SelectionSummary {
                    channel: SelectionChannel::Exact,
                    reason: "café match".into(),
                    role: Some(ContextRole::EditableSource),
                    lane: Some(RetrievalLane::ExactReference),
                },
                evidence: SelectionEvidence::default(),
                body: BodyRepresentation::SignatureOnly,
                span_ids: Vec::new(),
                source_handle: None,
                evidence_handle: None,
            }],
            candidate_audit: Vec::new(),
            spans: Vec::new(),
            relationships: Vec::new(),
            omissions: Vec::new(),
            capabilities: Vec::new(),
        }
    }

    #[test]
    fn empty_agent_and_evidence_outputs_match_exact_byte_goldens() {
        const AGENT: &str = "# RNA search context\n\n- projection: agent\n- intent: discover\n- body_policy: signature_only\n- selected_records: 0\n\n## Results\n\nNo selected records.\n\n## Render accounting\n\n- total: bytes=575 chars=575 estimated_tokens=144\n- estimate: unicode_chars_div_4_ceiling (deterministic estimate; not provider usage)\n- sections:\n  - headers: bytes=113 chars=113 estimated_tokens=29\n  - bodies: bytes=0 chars=0 estimated_tokens=0\n  - relationships: bytes=0 chars=0 estimated_tokens=0\n  - metadata: bytes=34 chars=34 estimated_tokens=9\n  - footer: bytes=428 chars=428 estimated_tokens=107\n";
        const EVIDENCE: &str = "# RNA search context\n\n- projection: evidence\n- intent: discover\n- body_policy: signature_only\n- selected_records: 0\n\n## Results\n\nNo selected records.\n\n## Render accounting\n\n- total: bytes=578 chars=578 estimated_tokens=145\n- estimate: unicode_chars_div_4_ceiling (deterministic estimate; not provider usage)\n- sections:\n  - headers: bytes=116 chars=116 estimated_tokens=29\n  - bodies: bytes=0 chars=0 estimated_tokens=0\n  - relationships: bytes=0 chars=0 estimated_tokens=0\n  - metadata: bytes=34 chars=34 estimated_tokens=9\n  - footer: bytes=428 chars=428 estimated_tokens=107\n";

        assert_eq!(
            render_projection(&empty_plan(SearchProjection::Agent))
                .unwrap()
                .text,
            AGENT
        );
        assert_eq!(
            render_projection(&empty_plan(SearchProjection::Evidence))
                .unwrap()
                .text,
            EVIDENCE
        );
    }

    #[test]
    fn unicode_signature_output_matches_exact_byte_golden() {
        const GOLDEN: &str = "# RNA search context\n\n- projection: agent\n- intent: discover\n- body_policy: signature_only\n- selected_records: 1\n\n## Results\n\n1. function `β` — [répo] src/ü.rs:1-1\n   - id: ` node:β `\n   - signature: ` fn β() `\n   - channel: exact; reason: café match\n   - body: signature_only\n   - role: editable_source\n   - lane: exact_reference\n\n## Render accounting\n\n- total: bytes=770 chars=762 estimated_tokens=191\n- estimate: unicode_chars_div_4_ceiling (deterministic estimate; not provider usage)\n- sections:\n  - headers: bytes=113 chars=113 estimated_tokens=29\n  - bodies: bytes=0 chars=0 estimated_tokens=0\n  - relationships: bytes=0 chars=0 estimated_tokens=0\n  - metadata: bytes=226 chars=218 estimated_tokens=55\n  - footer: bytes=431 chars=431 estimated_tokens=108\n";

        assert_eq!(
            render_projection(&unicode_signature_plan()).unwrap().text,
            GOLDEN
        );
    }

    #[test]
    fn evidence_output_with_candidate_audit_matches_exact_byte_golden() {
        const GOLDEN: &str = "# RNA search context\n\n- projection: evidence\n- intent: discover\n- body_policy: signature_only\n- selected_records: 1\n\n## Capability status\n\n- semantic_search: degraded — model=/tmp/private\n\n## Results\n\n1. function `β` — [répo] src/ü.rs:1-1\n   - id: ` node:β `\n   - signature: ` fn β() `\n   - channel: exact; reason: café match\n   - body: signature_only\n   - role: editable_source\n   - lane: exact_reference\n   - evidence.candidate_rank: 1\n   - evidence.content_hash: ` hash-β `\n   - evidence.score.vector_distance: 0.25\n   - evidence.provenance: vector — rank=1\n   - evidence.diagnostic.tie_break: stable_id\n\n## Candidate audit\n\n2. ` node:γ ` — no source; disposition=omitted; reason=outside limit\n   - evidence.candidate_rank: 2\n   - evidence.content_hash: ` hash-γ `\n   - evidence.score.lexical: 3\n\n## Render accounting\n\n- total: bytes=1250 chars=1233 estimated_tokens=309\n- estimate: unicode_chars_div_4_ceiling (deterministic estimate; not provider usage)\n- sections:\n  - headers: bytes=116 chars=116 estimated_tokens=29\n  - bodies: bytes=0 chars=0 estimated_tokens=0\n  - relationships: bytes=0 chars=0 estimated_tokens=0\n  - metadata: bytes=700 chars=683 estimated_tokens=171\n  - footer: bytes=434 chars=434 estimated_tokens=109\n";
        let mut input = unicode_signature_plan();
        input.request.projection = SearchProjection::Evidence;
        input.records[0].evidence = SelectionEvidence {
            raw_scores: BTreeMap::from([("vector_distance".into(), "0.25".into())]),
            content_hash: Some("hash-β".into()),
            candidate_rank: Some(1),
            provenance: vec![EvidenceProvenance {
                source: "vector".into(),
                detail: "rank=1".into(),
            }],
            diagnostics: BTreeMap::from([("tie_break".into(), "stable_id".into())]),
        };
        input.candidate_audit = vec![CandidateAudit {
            candidate_rank: 2,
            identity: RecordIdentity {
                node_id: "node:γ".into(),
                source: None,
            },
            disposition: CandidateDisposition::Omitted,
            reason: "outside limit".into(),
            evidence: SelectionEvidence {
                raw_scores: BTreeMap::from([("lexical".into(), "3".into())]),
                content_hash: Some("hash-γ".into()),
                candidate_rank: Some(2),
                ..Default::default()
            },
        }];
        input.capabilities = vec![CapabilityStatus {
            capability: "semantic_search".into(),
            state: CapabilityState::Degraded,
            detail: "model=/tmp/private".into(),
        }];

        assert_eq!(render_projection(&input).unwrap().text, GOLDEN);
    }

    #[test]
    fn agent_omits_evidence_but_handle_retains_it() {
        let action = render_projection(&plan(SearchProjection::Agent)).unwrap();
        assert!(!action.text.contains("secret-hash"));
        assert!(!action.text.contains("cosine"));
        assert!(!action.text.contains("audit-secret-hash"));
        assert!(!action.text.contains("outside bounded selection"));
        assert!(!action.text.contains("internal-scorer-path"));
        assert!(action.text.contains("- lsp: unavailable"));
        assert!(action.text.contains("hydrate_evidence"));
        let evidence = render_projection(&plan(SearchProjection::Evidence)).unwrap();
        assert!(evidence.text.contains("secret-hash"));
        assert!(evidence.text.contains("cosine"));
        assert!(evidence.text.contains("audit-secret-hash"));
        assert!(evidence.text.contains("outside bounded selection"));
        assert!(evidence.text.contains("internal-scorer-path"));
    }

    #[test]
    fn footer_accounts_for_itself_and_unicode() {
        let rendered = render_projection(&plan(SearchProjection::Agent)).unwrap();
        assert_eq!(rendered.text.len(), rendered.accounting.total.utf8_bytes);
        assert_eq!(
            rendered.text.chars().count(),
            rendered.accounting.total.unicode_chars
        );
        assert_eq!(
            rendered
                .accounting
                .sections
                .values()
                .map(|cost| cost.utf8_bytes)
                .sum::<usize>(),
            rendered.accounting.total.utf8_bytes
        );
        assert!(!rendered.accounting.provider_usage);
        assert_eq!(
            render_projection(&plan(SearchProjection::Agent))
                .unwrap()
                .text,
            rendered.text
        );
    }

    #[test]
    fn persisted_plan_reopens_to_byte_identical_output() {
        let original = plan(SearchProjection::Evidence);
        let bytes = serde_json::to_vec(&original).unwrap();
        let reopened: ProjectionPlan = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(
            render_projection(&original).unwrap().text.as_bytes(),
            render_projection(&reopened).unwrap().text.as_bytes()
        );
    }

    #[test]
    fn empty_agent_projection_is_explicit_and_accounted() {
        let empty = ProjectionPlan {
            request: ProjectionRequest::default(),
            records: Vec::new(),
            candidate_audit: Vec::new(),
            spans: Vec::new(),
            relationships: Vec::new(),
            omissions: Vec::new(),
            capabilities: Vec::new(),
        };

        let rendered = render_projection(&empty).unwrap();

        assert!(rendered.text.contains("No selected records."));
        assert!(rendered.text.contains("projection: agent"));
        assert_eq!(rendered.text.len(), rendered.accounting.total.utf8_bytes);
        assert!(!rendered.accounting.provider_usage);
    }

    #[test]
    fn impossible_budget_returns_a_typed_error() {
        let mut input = plan(SearchProjection::Agent);
        input.request.budget.max_rendered_bytes = Some(1);
        assert!(matches!(
            render_projection(&input),
            Err(RenderError::BudgetTooSmall { .. })
        ));
    }

    #[test]
    fn evidence_budget_deterministically_compacts_candidate_audit_before_bodies() {
        let mut input = plan(SearchProjection::Evidence);
        input.request.budget.max_rendered_bytes = Some(4_096);
        input.candidate_audit = (1..=200)
            .map(|rank| CandidateAudit {
                candidate_rank: rank,
                identity: RecordIdentity {
                    node_id: format!("candidate-{rank:02}"),
                    source: None,
                },
                disposition: CandidateDisposition::Omitted,
                reason: "bounded candidate was not selected; deterministic audit detail ".repeat(3),
                evidence: SelectionEvidence {
                    candidate_rank: Some(rank),
                    content_hash: Some(format!("hash-{rank:02}")),
                    diagnostics: BTreeMap::from([(
                        "tie_break".into(),
                        "stable identity after calibrated channel ranks".into(),
                    )]),
                    ..Default::default()
                },
            })
            .collect();

        let rendered = render_projection(&input).unwrap();

        assert!(rendered.text.len() <= 4_096);
        assert!(!rendered.text.contains("## Candidate audit"));
        assert!(rendered.text.contains("candidate audit omitted"));
        assert!(rendered.plan.candidate_audit.is_empty());
        assert_eq!(rendered.plan.spans.len(), 1);
        assert_eq!(rendered, render_projection(&input).unwrap());
    }

    #[test]
    fn graph_delta_budget_preserves_capability_states_before_duplicate_audit() {
        let mut input = plan(SearchProjection::Evidence);
        input.request.budget.max_rendered_bytes = Some(4_096);
        input.records[0].selection.role = Some(ContextRole::ProposalDelta);
        input.capabilities = [
            "graph_delta_proposal_parsing",
            "graph_delta_live_graph_inference",
            "graph_delta_route_analysis",
            "graph_delta_card_coverage",
            "graph_delta_changed_files",
            "graph_delta_affected_locus_checklist",
            "proposal_overlay_persistence",
        ]
        .into_iter()
        .map(|capability| CapabilityStatus {
            capability: capability.into(),
            state: CapabilityState::Ready,
            detail: "verbose graph-delta diagnostic detail ".repeat(20),
        })
        .collect();
        input.candidate_audit = (1..=100)
            .map(|rank| CandidateAudit {
                candidate_rank: rank,
                identity: RecordIdentity {
                    node_id: format!("candidate-{rank:02}"),
                    source: None,
                },
                disposition: CandidateDisposition::Omitted,
                reason: "duplicate graph-delta candidate audit ".repeat(10),
                evidence: SelectionEvidence::default(),
            })
            .collect();

        let rendered = render_projection(&input).unwrap();

        assert!(rendered.accounting.total.utf8_bytes <= 4_096);
        assert!(rendered.plan.candidate_audit.is_empty());
        assert!(!rendered.text.contains("delivery_capabilities"));
        for capability in &input.capabilities {
            assert!(
                rendered
                    .text
                    .contains(&format!("{}: ready", capability.capability)),
                "missing capability {}",
                capability.capability
            );
        }
        assert_eq!(rendered, render_projection(&input).unwrap());
    }

    #[test]
    fn evidence_budget_preserves_selected_identities_while_omitting_duplicate_audit_detail() {
        let mut input = plan(SearchProjection::Evidence);
        input.request.budget.max_rendered_bytes = Some(6_000);
        input.spans.clear();
        input.records = (0..8)
            .map(|rank| {
                let mut record = input.records[0].clone();
                record.selection_rank = rank;
                record.identity.node_id = format!("selected-{rank}");
                record.evidence.diagnostics.insert(
                    "complete_audit".into(),
                    format!("record-{rank}-").repeat(120),
                );
                record
            })
            .collect();

        let rendered = render_projection(&input).unwrap();

        assert!(rendered.text.len() <= 6_000);
        assert_eq!(rendered.plan.records.len(), 8);
        assert!(rendered.plan.candidate_audit.is_empty());
        assert!(rendered
            .plan
            .records
            .iter()
            .any(|record| record.evidence == SelectionEvidence::default()));
        assert!(rendered
            .text
            .contains("selected record audit detail omitted"));
        assert_eq!(rendered, render_projection(&input).unwrap());
    }

    #[test]
    fn non_graph_record_does_not_emit_a_dead_evidence_handle() {
        let mut input = plan(SearchProjection::Agent);
        input.records[0].evidence_handle = None;

        let rendered = render_projection(&input).unwrap();

        assert!(!rendered.text.contains("hydrate_evidence:"));
        assert!(rendered.text.contains("hydrate_source:"));
    }

    #[test]
    fn flat_non_body_metadata_degrades_below_five_thousand_tokens() {
        let mut input = plan(SearchProjection::Evidence);
        input.request.body_policy = BodyPolicy::SignatureOnly;
        input.request.budget.max_estimated_tokens = Some(5_000);
        input.spans.clear();
        input.records = (0..120)
            .map(|rank| {
                let mut record = input.records[0].clone();
                record.selection_rank = rank;
                record.identity.node_id = format!("record-{rank:03}");
                record.symbol.name = format!("symbol_{rank:03}");
                record.symbol.declared_metadata.insert(
                    "verbose".into(),
                    "server-owned diagnostic metadata ".repeat(30),
                );
                record
                    .evidence
                    .diagnostics
                    .insert("ranking".into(), "complete ranking diagnostics ".repeat(30));
                record
            })
            .collect();
        input.candidate_audit = (0..120)
            .map(|rank| CandidateAudit {
                candidate_rank: rank,
                identity: RecordIdentity {
                    node_id: format!("audit-{rank:03}"),
                    source: None,
                },
                disposition: CandidateDisposition::Omitted,
                reason: "verbose audit reason ".repeat(30),
                evidence: SelectionEvidence::default(),
            })
            .collect();

        let rendered = render_projection(&input).unwrap();
        assert!(rendered.accounting.total.estimated_tokens <= 5_000);
        assert!(!rendered.plan.records.is_empty());
        assert!(rendered.plan.records.len() < 120);
        assert!(rendered.plan.records[0].source_handle.is_some());
        assert!(
            rendered
                .plan
                .omissions
                .iter()
                .any(|omission| omission.detail.contains("hydrate=rna-h2:")),
            "an omitted flat-tail record must retain a compact hydration handle"
        );
        assert!(rendered.text.contains("hydrate=rna-h2:"));
        assert_eq!(rendered, render_projection(&input).unwrap());
    }

    #[test]
    fn flat_record_reason_compaction_reaches_a_terminal_marker() {
        let mut input = plan(SearchProjection::Evidence);
        input.request.intent = SearchIntent::Discover;
        input.records[0].selection.reason = "ordinary search ranking diagnostics ".repeat(10);
        input.records[0].symbol.declared_metadata.clear();
        input.records[0].symbol.extraction_source = None;

        assert!(compact_record_metadata(&mut input));
        assert_eq!(
            input.records[0].selection.reason,
            "selected; hydrate for detail"
        );
        assert!(
            !compact_record_metadata(&mut input),
            "the compact marker must not remain eligible for another pass"
        );
    }

    #[test]
    fn task_fixed_sections_degrade_below_twenty_four_thousand_bytes() {
        let mut input = plan(SearchProjection::Evidence);
        input.request.intent = SearchIntent::Implement;
        input.request.budget.max_rendered_bytes = Some(24_000);
        input.spans.clear();
        input.records = (0..12)
            .map(|rank| {
                let mut record = input.records[0].clone();
                record.selection_rank = rank;
                record.identity.node_id = format!("task-record-{rank:02}");
                record.symbol.name = format!("actionable_task_symbol_{rank:02}");
                record.selection.reason = format!(
                    "retrieval detail {}; quality=actionable; obligations={{concept:task-{rank:02}}}",
                    "server-owned ".repeat(20)
                );
                record
            })
            .collect();
        input.capabilities = (0..200)
            .map(|rank| CapabilityStatus {
                capability: format!("task_lane_{rank:03}"),
                state: CapabilityState::Ready,
                detail: "server-owned lane diagnostics ".repeat(20),
            })
            .collect();
        input.candidate_audit = (0..200)
            .map(|rank| CandidateAudit {
                candidate_rank: rank,
                identity: RecordIdentity {
                    node_id: format!("candidate-{rank:03}"),
                    source: None,
                },
                disposition: CandidateDisposition::Omitted,
                reason: "server-owned candidate diagnostics ".repeat(20),
                evidence: SelectionEvidence::default(),
            })
            .collect();

        let rendered = render_projection(&input).unwrap();
        assert!(rendered.accounting.total.utf8_bytes <= 24_000);
        assert_eq!(
            rendered.plan.records.len(),
            12,
            "task evidence is protected"
        );
        assert!(rendered.plan.records[0].source_handle.is_some());
        assert_eq!(rendered, render_projection(&input).unwrap());
    }

    #[test]
    fn task_metadata_compaction_progresses_to_later_degradation_stages() {
        let mut input = plan(SearchProjection::Evidence);
        input.request.intent = SearchIntent::Implement;
        input.request.budget.max_rendered_bytes = Some(2_500);
        input.records[0].selection.reason = super::super::task_record_selection_reason(
            &"verbose retrieval detail ".repeat(30),
            "coverage per exact rendered cost",
            &super::super::task_context::EvidenceQuality::Actionable,
            &BTreeSet::from([
                "concept:override".to_string(),
                "structure:EditableSource:override".to_string(),
            ]),
        );
        input.relationships.push(ProjectedRelationship {
            from: input.records[0].identity.node_id.clone(),
            kind: "depends_on".into(),
            to: "node:dependency".into(),
            reason: "verbose relationship detail ".repeat(40),
        });
        input.spans[0].text = "task source body ".repeat(500);

        let rendered = render_projection(&input).unwrap();

        assert!(rendered.accounting.total.utf8_bytes <= 2_500);
        assert_eq!(rendered.plan.records.len(), 1);
        assert_eq!(
            rendered.plan.records[0].selection.reason,
            "quality=actionable; obligations_visible=none; obligations_hydrate=concept:override,structure:EditableSource:override"
        );
        assert!(
            rendered.plan.relationships.is_empty()
                || rendered.plan.relationships[0].reason.is_empty()
        );
        assert_eq!(rendered, render_projection(&input).unwrap());
    }

    #[test]
    fn task_metadata_compaction_retains_the_producer_obligation_floor() {
        let mut input = plan(SearchProjection::Evidence);
        input.request.intent = SearchIntent::Implement;
        input.records[0].selection.role = Some(ContextRole::Test);
        input.records[0].symbol.kind = "function".into();
        input.records[0].symbol.signature = "fn test_override()".into();
        input.records[0].selection.reason = "retrieval; coverage; quality=actionable; obligations=concept:override,structure:Test:override,validation:task-relevant-tests".into();

        assert_eq!(
            compact_task_selection_reason(&input, 0),
            "quality=actionable; obligations_visible=concept:override,structure:Test:override,validation:task-relevant-tests; obligations_hydrate=none"
        );
    }

    #[test]
    fn task_obligation_carrier_excerpt_is_bounded_visible_and_deterministic() {
        let mut input = plan(SearchProjection::Evidence);
        input.request.intent = SearchIntent::Implement;
        input.request.body_policy = BodyPolicy::FocusedSpan;
        input.request.budget.max_rendered_bytes = Some(5_000);
        input.candidate_audit = (0..80)
            .map(|rank| CandidateAudit {
                candidate_rank: rank,
                identity: RecordIdentity {
                    node_id: format!("candidate-{rank}"),
                    source: None,
                },
                disposition: CandidateDisposition::Omitted,
                reason: "server-owned candidate diagnostics ".repeat(20),
                evidence: SelectionEvidence::default(),
            })
            .collect();
        let reason = "retrieval; coverage; quality=actionable; obligations=concept:annotated,concept:notrequired,concept:override,structure:EditableSource:annotated+notrequired+override";
        input.records[0].selection.reason = reason.into();
        input.records[0].symbol.signature = "fn generate_mapping()".into();
        input.spans[0].mappings[0].selection.reason = reason.into();
        input.spans[0].text = format!(
            "{}\nif explicit_override is not None {{\n    return explicit_override; // explicit precedence\n}}\n{}\nif is_notrequired(annotation) {{\n    annotation = unwrap_notrequired(annotation);\n}}\nif is_annotated(annotation) {{\n    effective.overrides = annotation_override(annotation);\n}}\n{}",
            "unrelated setup line\n".repeat(300),
            "unrelated middle line\n".repeat(300),
            "unrelated tail line\n".repeat(300),
        );

        let rendered = render_projection(&input).unwrap();

        assert!(rendered.accounting.total.utf8_bytes <= 5_000);
        assert!(rendered.text.contains("explicit precedence"));
        assert!(rendered.text.contains("unwrap_notrequired"));
        assert!(rendered.text.contains("effective.overrides"));
        assert!(rendered.text.contains(
            "obligations_visible=concept:annotated,concept:notrequired,concept:override,structure:EditableSource:annotated+notrequired+override"
        ));
        assert!(!rendered.text.contains("unrelated setup line\nunrelated setup line"));
        assert_eq!(
            rendered.plan.spans[0].representation,
            BodyRepresentation::Truncated
        );
        assert!(rendered.plan.records[0].source_handle.is_some());
        assert_eq!(rendered, render_projection(&input).unwrap());
    }

    #[test]
    fn compact_task_obligations_distinguish_visible_from_hydration_only() {
        let mut input = plan(SearchProjection::Evidence);
        input.request.intent = SearchIntent::Implement;
        input.spans.clear();
        input.records[0].symbol.signature = "fn unrelated()".into();
        input.records[0].selection.reason =
            "retrieval; quality=actionable; obligations=concept:override".into();

        assert_eq!(
            compact_task_selection_reason(&input, 0),
            "quality=actionable; obligations_visible=none; obligations_hydrate=concept:override"
        );
    }

    #[test]
    fn cattrs_fixture_packet_retains_every_actionable_obligation() {
        let mut input = plan(SearchProjection::Evidence);
        input.request.intent = SearchIntent::Implement;
        input.request.body_policy = BodyPolicy::SignatureOnly;
        input.request.budget.max_estimated_tokens = Some(6_000);
        input.spans.clear();
        let fixtures = [
            (
                "cattrs/gen/_override.py",
                "AttributeOverride",
                "class AttributeOverride: ...",
            ),
            (
                "cattrs/gen/_generics.py",
                "make_dict_structure_fn",
                "generate attrs and dataclass structure hooks",
            ),
            (
                "cattrs/gen/typeddicts.py",
                "make_typeddict_fn",
                "NotRequired[Annotated[T, metadata]]",
            ),
            (
                "cattrs/gen/tuples.py",
                "make_namedtuple_fn",
                "dict-style NamedTuple generation",
            ),
            (
                "cattrs/converters.py",
                "effective_overrides",
                "explicit precedence; effective .overrides",
            ),
            (
                "tests/test_generics.py",
                "test_override_generation",
                "assert override TypedDict NamedTuple generation",
            ),
        ];
        input.records = fixtures
            .into_iter()
            .enumerate()
            .map(|(rank, (path, name, signature))| {
                let mut record = input.records[0].clone();
                record.selection_rank = rank;
                record.identity.node_id = format!("cattrs:{path}:{name}");
                record.identity.source = Some(SourceSpan {
                    root: "cattrs".into(),
                    path: path.into(),
                    start_line: 1,
                    end_line: 8,
                });
                record.symbol.name = name.into();
                record.symbol.signature = signature.into();
                record.selection.role = Some(if path.starts_with("tests/") {
                    ContextRole::Test
                } else {
                    ContextRole::EditableSource
                });
                record
            })
            .collect();
        let rendered = render_projection(&input).unwrap();
        assert!(rendered.accounting.total.estimated_tokens <= 6_000);
        assert_eq!(rendered.plan.records.len(), fixtures.len());
        for needle in [
            "AttributeOverride",
            "attrs and dataclass",
            "NotRequired[Annotated",
            "dict-style NamedTuple",
            "effective .overrides",
            "test_override_generation",
        ] {
            assert!(
                rendered.text.contains(needle),
                "missing actionable evidence {needle}"
            );
        }
    }
}
