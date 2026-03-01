# ATS Scoring Methodology

Detailed algorithms and edge cases for enterprise-grade ATS matching.

## Table of Contents

1. [Category Scoring Details](#category-scoring-details)
2. [Weighting Adjustments](#weighting-adjustments)
3. [Edge Case Handling](#edge-case-handling)
4. [Confidence Calculation](#confidence-calculation)

## Category Scoring Details

### Hard Skills Scoring (25%)

**Algorithm**:
```
Required Skills Score = (Matched Required / Total Required) × 70%
Preferred Skills Score = (Matched Preferred / Total Preferred) × 30%
Hard Skills Score = Required Skills Score + Preferred Skills Score
```

**Match Quality Multipliers**:
| Match Type | Multiplier | Example |
|------------|------------|---------|
| Exact match | 1.0 | "Python" = "Python" |
| Case-insensitive | 1.0 | "python" = "Python" |
| Synonym | 0.85 | "K8s" = "Kubernetes" |
| Partial/Related | 0.5 | "React Native" ≈ "React" |
| Transferable | 0.3 | "Vue.js" → "React" (both frontend frameworks) |

**Skill Extraction Patterns**:
- Explicit skills sections
- Technologies mentioned in bullet points
- Tools referenced in achievements
- Certifications implying skills

### Experience Scoring (20%)

**Years Calculation**:
```
Experience Score = min(1.0, Candidate Years / Required Years) × 100

If Candidate Years > Required Years × 1.5:
    Flag: "Potential overqualification"
```

**Seniority Alignment Matrix**:
| Required | Junior | Mid | Senior | Staff | Director |
|----------|--------|-----|--------|-------|----------|
| Junior | 100% | 80% | 60% | 40% | 30% |
| Mid | 60% | 100% | 90% | 70% | 50% |
| Senior | 30% | 70% | 100% | 95% | 80% |
| Staff | 20% | 50% | 85% | 100% | 90% |
| Director | 10% | 30% | 60% | 85% | 100% |

**Scope Scoring**:
- Team size alignment: ±20% tolerance for full credit
- Budget responsibility: Direct experience preferred
- Geographic scope: International > National > Regional

### Keywords Scoring (15%)

**Frequency-Weighted Algorithm**:
```
For each JD keyword:
    JD_frequency = count in JD
    Resume_present = 1 if found, 0 if not
    Keyword_score = Resume_present × log(1 + JD_frequency)

Keywords Score = Σ(Keyword_scores) / Σ(Max possible scores) × 100
```

**Keyword Categories** (by importance):
1. Job title keywords (weight: 2.0)
2. Technical requirements (weight: 1.5)
3. Industry terms (weight: 1.2)
4. Soft skill indicators (weight: 1.0)
5. Nice-to-have terms (weight: 0.8)

### Job Titles Scoring (12%)

**Title Alignment Algorithm**:
```
Title Score = (Role Match × 0.5) + (Level Match × 0.3) + (Domain Match × 0.2)

Role Match: How similar is the function (Engineering, Product, Design)
Level Match: How aligned is seniority (Head, Director, Senior, etc.)
Domain Match: How relevant is the specialization
```

**Common Title Mappings**:
| JD Title | Equivalent Titles |
|----------|-------------------|
| Head of Engineering | VP Engineering, Engineering Director, CTO (startup) |
| Senior Engineer | Staff Engineer, Lead Engineer, Principal (some orgs) |
| Engineering Manager | Team Lead, Tech Lead Manager, Dev Manager |
| Product Manager | Product Owner, Program Manager (some contexts) |

### Soft Skills Scoring (10%)

**Evidence-Based Scoring**:
```
For each required soft skill:
    If explicit mention with evidence: 100%
    If demonstrated through achievements: 80%
    If implied by role: 50%
    If not found: 0%
```

**Soft Skill Indicators**:
| Soft Skill | Evidence Patterns |
|------------|-------------------|
| Leadership | "led", "managed", "directed", team size mentioned |
| Communication | "presented", "stakeholder", "cross-functional" |
| Problem-solving | "resolved", "improved", "optimized", metrics |
| Collaboration | "partnered", "worked with", "coordinated" |
| Innovation | "introduced", "pioneered", "first to" |

### Education Scoring (10%)

**Degree Hierarchy**:
```
PhD > Master's > Bachelor's > Associate's > Certification > Bootcamp

Score = Candidate Level / Required Level × 100
(capped at 100%, excess education doesn't boost score)
```

**Field Relevance**:
| Requirement | Direct Match | Related | Unrelated |
|-------------|--------------|---------|-----------|
| CS/Engineering | 100% | 80% | 50% |
| Business/MBA | 100% | 75% | 50% |
| Any degree | 100% | 100% | 100% |

**Certification Scoring**:
- Required cert present: 100%
- Equivalent cert: 80%
- Expired cert: 50%
- In-progress: 30%
- Missing required: 0%

### Industry Scoring (8%)

**Domain Relevance Matrix**:
```
Direct experience in same industry: 100%
Adjacent industry experience: 70%
Transferable industry experience: 40%
No relevant industry experience: 10%
```

**Industry Adjacency Examples**:
| Target Industry | Adjacent Industries |
|-----------------|---------------------|
| E-commerce | Retail, Marketplace, FinTech |
| FinTech | Banking, E-commerce, Insurance |
| Healthcare | Biotech, Insurance, Wellness |
| SaaS | Enterprise Software, Cloud, DevTools |

## Weighting Adjustments

### Role-Based Weight Modifications

For highly technical roles (Staff+, Principal, Architect):
```
Hard Skills: 30% (+5%)
Experience: 25% (+5%)
Keywords: 12% (-3%)
Job Titles: 10% (-2%)
Soft Skills: 8% (-2%)
Education: 8% (-2%)
Industry: 7% (-1%)
```

For leadership roles (Director+, VP, Head):
```
Hard Skills: 20% (-5%)
Experience: 25% (+5%)
Keywords: 12% (-3%)
Job Titles: 15% (+3%)
Soft Skills: 15% (+5%)
Education: 8% (-2%)
Industry: 5% (-3%)
```

## Edge Case Handling

### Sparse JD
When JD has fewer than 5 requirements:
- Increase weight of available requirements
- Set confidence to "Low"
- Note: "Limited JD data - scores may be less precise"

### Career Changers
When industry mismatch detected:
- Emphasize transferable skills (boost by 20%)
- Look for industry-adjacent experience
- Highlight relevant certifications
- Note: "Career transition detected - focus on transferable skills"

### Overqualified Candidates
When experience exceeds requirements by >50%:
- Flag potential overqualification
- Suggest tailoring resume down
- Note: "Consider emphasizing aspects aligned to role level"

### Gaps in Employment
When gaps detected:
- Don't penalize directly
- Note for user awareness
- Suggest addressing in cover letter if >6 months

### Non-Traditional Background
When formal education missing but experience strong:
- Boost experience weight by 5%
- Reduce education weight by 5%
- Note: "Experience-based profile - education weighted less"

## Confidence Calculation

```
Base Confidence = 100%

Deductions:
- JD < 200 words: -20%
- Resume < 300 words: -15%
- No explicit skills section: -10%
- No dates on experience: -10%
- Ambiguous requirements: -5% each
- Non-standard formatting: -10%

Final Confidence = max(30%, Base - Deductions)
```

**Confidence Levels**:
| Range | Level | Interpretation |
|-------|-------|----------------|
| 80-100% | High | Reliable analysis |
| 60-79% | Medium | Generally accurate, some uncertainty |
| 30-59% | Low | Best effort, verify manually |
