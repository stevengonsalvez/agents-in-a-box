---
name: ats-resume-matcher
description: |
  Professional ATS (Applicant Tracking System) resume matching and scoring tool that operates with the precision of enterprise systems like Greenhouse, Lever, Workday, and Breezy HR. Use this skill when:
  (1) Matching a resume against a job description to calculate fit scores
  (2) Analyzing resume-JD alignment with detailed category breakdowns
  (3) Identifying gaps between candidate qualifications and job requirements
  (4) Getting actionable suggestions to improve resume match percentage
  (5) Preparing a resume for ATS optimization before job applications
  Supports PDF, DOCX, Markdown, and plain text inputs for both resumes and job descriptions.
---

# ATS Resume Matcher

Enterprise-grade resume-to-job-description matching with detailed scoring, gap analysis, and optimization suggestions.

## Quick Start

When given a resume and job description:

1. Extract and parse both documents
2. Run the 7-category ATS analysis
3. Output the structured match report
4. Provide prioritized optimization suggestions

## Input Handling

### Supported Formats

| Format | Resume | Job Description |
|--------|--------|-----------------|
| PDF | Yes (extract text) | Yes |
| DOCX | Yes (extract text) | Yes |
| Markdown | Yes | Yes |
| Plain text | Yes | Yes |
| Pasted content | Yes | Yes |

### Extraction Priority

1. If file path provided → Read and extract text
2. If pasted content → Parse directly
3. If URL provided → Fetch and extract

## ATS Matching Algorithm

### Category Weights (Total: 100%)

| Category | Weight | Description |
|----------|--------|-------------|
| **Hard Skills** | 25% | Technical skills, tools, technologies, languages |
| **Experience** | 20% | Years of experience, seniority level, scope |
| **Keywords** | 15% | Exact phrase matches, industry terminology |
| **Job Titles** | 12% | Title alignment, progression, relevance |
| **Soft Skills** | 10% | Leadership, communication, collaboration |
| **Education** | 10% | Degrees, certifications, training |
| **Industry** | 8% | Domain experience, sector knowledge |

### Scoring Methodology

For each category, calculate:

```
Category Score = (Matched Items / Required Items) × 100

Weighted Score = Category Score × Category Weight

Overall Match = Σ(All Weighted Scores)
```

#### Match Classification

| Score Range | Classification | Interpretation |
|-------------|----------------|----------------|
| 85-100% | Excellent Match | Strong candidate, likely to pass ATS |
| 70-84% | Good Match | Competitive candidate, minor gaps |
| 55-69% | Moderate Match | Some gaps, optimization recommended |
| 40-54% | Weak Match | Significant gaps, major revision needed |
| 0-39% | Poor Match | Role mismatch or major skill gaps |

## Analysis Process

### Step 1: Parse Job Description

Extract from JD:
- **Required skills** (must-have)
- **Preferred skills** (nice-to-have)
- **Experience requirements** (years, level)
- **Education requirements** (degree, field)
- **Certifications** (required/preferred)
- **Job title keywords**
- **Industry/domain terms**
- **Soft skill indicators**

Classify each requirement as:
- `REQUIRED` - Explicitly stated as required/must-have
- `PREFERRED` - Stated as preferred/nice-to-have/bonus
- `IMPLIED` - Inferred from context

### Step 2: Parse Resume

Extract from resume:
- **Technical skills** (explicit and demonstrated)
- **Work experience** (titles, duration, scope)
- **Education** (degrees, institutions, dates)
- **Certifications** (names, dates, status)
- **Achievements** (quantified results)
- **Industry exposure** (domains worked in)
- **Keywords** (terminology used)

### Step 3: Match Analysis

For each JD requirement, find resume evidence:

```
EXACT MATCH    → 100% credit (keyword appears exactly)
SYNONYM MATCH  → 85% credit (equivalent term used)
PARTIAL MATCH  → 50% credit (related but not equivalent)
TRANSFERABLE   → 30% credit (skill could apply)
NO MATCH       → 0% credit (not found)
```

### Step 4: Generate Report

See output format below.

## Output Format

Generate this exact structure:

```markdown
# ATS Match Report

## Overall Score: [XX]% - [Classification]

**Resume**: [filename or "Provided content"]
**Position**: [Job title from JD]
**Company**: [Company name if available]
**Analysis Date**: [Current date]

---

## Score Breakdown

| Category | Score | Weight | Weighted | Status |
|----------|-------|--------|----------|--------|
| Hard Skills | XX% | 25% | X.X | [✓/⚠/✗] |
| Experience | XX% | 20% | X.X | [✓/⚠/✗] |
| Keywords | XX% | 15% | X.X | [✓/⚠/✗] |
| Job Titles | XX% | 12% | X.X | [✓/⚠/✗] |
| Soft Skills | XX% | 10% | X.X | [✓/⚠/✗] |
| Education | XX% | 10% | X.X | [✓/⚠/✗] |
| Industry | XX% | 8% | X.X | [✓/⚠/✗] |
| **TOTAL** | | **100%** | **XX.X** | |

Status: ✓ = 70%+, ⚠ = 50-69%, ✗ = <50%

---

## Detailed Analysis

### Hard Skills (XX%)

**Matched (X/Y required)**:
- ✓ [Skill] - Found: "[evidence from resume]"
- ✓ [Skill] - Found: "[evidence from resume]"

**Partial Matches**:
- ⚠ [Required skill] → [Related skill found] (XX% credit)

**Missing**:
- ✗ [Skill] - REQUIRED - Not found
- ✗ [Skill] - PREFERRED - Not found

### Experience (XX%)

**Requirements**:
- Required: [X] years in [domain]
- Found: [Y] years in [domain]
- Match: [Exceeds/Meets/Below] requirement

**Seniority Alignment**:
- Required level: [Senior/Mid/Junior]
- Demonstrated level: [Senior/Mid/Junior]
- Gap: [None/Minor/Significant]

**Scope Match**:
- Required: [team size, budget, scale from JD]
- Demonstrated: [evidence from resume]

### Keywords (XX%)

**Exact Matches (X/Y)**:
- ✓ "[keyword]" - Found [X] times
- ✓ "[keyword]" - Found [X] times

**Missing High-Value Keywords**:
- ✗ "[keyword]" - Appears [X] times in JD
- ✗ "[keyword]" - Industry-standard term

### Job Titles (XX%)

**Title Progression Analysis**:
| Your Title | Target Title | Alignment |
|------------|--------------|-----------|
| [Current] | [JD Title] | XX% |

**Title Keywords**:
- ✓ [Matched title keyword]
- ✗ [Missing title keyword]

### Soft Skills (XX%)

**Demonstrated**:
- ✓ [Soft skill] - Evidence: "[quote from resume]"

**Required but Missing**:
- ✗ [Soft skill] - Add evidence of this skill

### Education (XX%)

**Degree Match**:
- Required: [Degree] in [Field]
- Found: [Degree] in [Field]
- Status: [Meets/Exceeds/Below/Alternative]

**Certifications**:
- ✓ [Cert name] - Matches requirement
- ✗ [Required cert] - Not found

### Industry (XX%)

**Domain Experience**:
- Required: [Industry/Domain]
- Found: [Industries in resume]
- Relevance: [Direct/Adjacent/Transferable]

---

## Gap Summary

### Critical Gaps (Address First)
1. **[Gap]** - Impact: High - [Brief explanation]
2. **[Gap]** - Impact: High - [Brief explanation]

### Important Gaps
1. **[Gap]** - Impact: Medium - [Brief explanation]

### Minor Gaps
1. **[Gap]** - Impact: Low - [Brief explanation]

---

## Optimization Suggestions

### High Impact (Estimated +X-Y% improvement)

1. **[Suggestion title]**
   - Current: [What's in resume now]
   - Recommended: [What to add/change]
   - Where: [Which section to modify]
   - Example: "[Specific wording to consider]"

2. **[Suggestion title]**
   - Current: [What's in resume now]
   - Recommended: [What to add/change]
   - Where: [Which section to modify]
   - Example: "[Specific wording to consider]"

### Medium Impact (Estimated +X-Y% improvement)

1. **[Suggestion]**
   - [Details]

### Quick Wins (Estimated +X-Y% improvement)

1. **[Suggestion]** - [One-line actionable item]
2. **[Suggestion]** - [One-line actionable item]

---

## ATS Compatibility Notes

**Formatting Issues**:
- [Any detected formatting issues that might cause ATS parsing problems]

**Keyword Density**:
- Top JD keywords not in resume: [list]
- Recommendation: [specific advice]

**Section Headers**:
- [Any non-standard headers that ATS might not recognize]

---

## Confidence Level

Analysis confidence: [High/Medium/Low]
- [Reason for confidence level]
```

## Keyword Matching Rules

### Skill Synonyms

Apply these common equivalences:

| JD Term | Also Accept |
|---------|-------------|
| JavaScript | JS, ECMAScript, ES6+ |
| Python | Python3, Py |
| Machine Learning | ML, Deep Learning, AI/ML |
| Amazon Web Services | AWS, Amazon Cloud |
| Google Cloud Platform | GCP, Google Cloud |
| Microsoft Azure | Azure, MS Azure |
| Continuous Integration | CI, CI/CD |
| Continuous Deployment | CD, CI/CD |
| Kubernetes | K8s, K8 |
| PostgreSQL | Postgres, PSQL |
| MongoDB | Mongo |
| React.js | React, ReactJS |
| Node.js | Node, NodeJS |
| TypeScript | TS |
| GraphQL | GQL |
| REST API | RESTful, REST |
| Agile | Scrum, Kanban, Agile/Scrum |
| Project Management | PM, Program Management |
| People Management | Team Leadership, Engineering Management |

### Experience Level Mapping

| JD Requirement | Acceptable Range |
|----------------|------------------|
| Entry level | 0-2 years |
| Junior | 1-3 years |
| Mid-level | 3-5 years |
| Senior | 5-8 years |
| Staff/Principal | 8-12 years |
| Director | 10+ years |
| VP/Head | 12+ years |

### Education Equivalences

| Requirement | Also Accept |
|-------------|-------------|
| Bachelor's required | Master's, PhD |
| Master's preferred | PhD, Bachelor's + 2 years |
| CS degree | Related technical degree + experience |
| MBA | Business degree + experience |

## Suggestion Generation Rules

### Prioritization

Generate suggestions in this priority order:

1. **Missing REQUIRED skills** - Highest impact
2. **Experience gaps** - High impact
3. **Missing exact keywords** - Medium-high impact
4. **Missing certifications** - Medium impact
5. **Soft skill evidence** - Medium impact
6. **Keyword optimization** - Low-medium impact
7. **Formatting improvements** - Low impact

### Suggestion Quality

Each suggestion must be:
- **Specific** - Not generic advice
- **Actionable** - Clear what to do
- **Evidenced** - Based on actual gap found
- **Realistic** - Achievable without lying

### Example Suggestion Patterns

**For missing skill**:
```
Add [SKILL] to your skills section. Based on your experience with
[RELATED SKILL], you may have exposure to this. If you have any
experience, add a bullet point like: "Utilized [SKILL] for [USE CASE]"
```

**For keyword gap**:
```
The JD mentions "[KEYWORD]" [X] times. Consider incorporating this
terminology in your [SECTION]. For example, change "[CURRENT PHRASING]"
to "[SUGGESTED PHRASING WITH KEYWORD]"
```

**For experience gap**:
```
The role requires [X] years of [DOMAIN] experience. Your resume shows
[Y] years. Emphasize your [RELEVANT EXPERIENCE] and quantify impact
to demonstrate equivalent depth.
```

## Edge Cases

### When Resume > JD Requirements
- Still note as "Exceeds" not 100%+
- Flag potential overqualification concerns
- Suggest tailoring to avoid rejection

### When JD is Vague
- Note confidence as "Medium" or "Low"
- Make reasonable inferences
- List assumptions made

### When Resume Has Non-Standard Format
- Extract what's possible
- Note any parsing limitations
- Provide best-effort analysis

## References

For detailed information, see:
- [references/scoring-methodology.md](references/scoring-methodology.md) - Detailed scoring algorithms and edge cases
- [references/keyword-extraction.md](references/keyword-extraction.md) - Extended synonym lists and extraction patterns
