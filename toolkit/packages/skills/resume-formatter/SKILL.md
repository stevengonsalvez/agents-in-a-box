---
name: resume-formatter
description: |
  Professional resume formatting and PDF generation tool. Use this skill when:
  (1) Creating a new resume tailored to a specific job description
  (2) Converting an existing resume to the standard HTML template
  (3) Generating a PDF from an HTML resume
  (4) Updating resume content while maintaining consistent formatting
  Produces professional, ATS-friendly resumes with consistent blue-themed styling.
---

# Resume Formatter

Generate professional resumes with consistent styling and PDF output.

## Quick Start

1. Create/update resume content (markdown or direct HTML)
2. Apply the HTML template from `assets/resume_template.html`
3. Generate PDF using `scripts/generate_pdf.py`

## Workflow

### Creating a New Resume

1. **Gather requirements**: Job description, target company, role title
2. **Draft content** in markdown with sections: Profile, Experience, Skills, Education
3. **Convert to HTML** using the template structure from `assets/resume_template.html`
4. **Generate PDF**: `python scripts/generate_pdf.py resume.html -o resume.pdf`

### Updating Existing Resume

1. Read the existing HTML file
2. Modify content sections as needed
3. Regenerate PDF

## Template Structure

The HTML template uses these CSS classes:

| Element | Class | Purpose |
|---------|-------|---------|
| Page wrapper | `.container` | Centers content, sets max-width |
| Name/contact | `.header`, `.contact-info` | Centered header section |
| Section titles | `.section-header` | Blue-accented section headers |
| Profile text | `.profile` | Summary paragraph |
| Job entry | `.role-card` | Blue left-border card for each role |
| Job title row | `.job-header`, `.job-title`, `.company-name`, `.location-date` | Flex layout |
| Achievements | `.achievements li` | Blue arrow bullets with `<strong>` headlines |
| Metrics box | `.scale-snapshot` | Highlighted metrics summary |
| Skills | `.skills-grid`, `.skill-category`, `.skill-label`, `.skill-items` | Inline label + items |
| Education | `.education-entry`, `.degree`, `.university`, `.years` | Stacked text |

### Experience Card Pattern

```html
<div class="role-card">
    <div class="job-header">
        <div class="job-title-company">
            <h3 class="job-title">Head of Engineering</h3>
            <span class="company-name">Company Name</span>
        </div>
        <div class="location-date">London, UK • May 2023–Present</div>
    </div>
    <ul class="achievements">
        <li><strong>Achievement headline</strong>: Description with metrics and impact.</li>
    </ul>
    <div class="scale-snapshot">
        Scale snapshot: £Xm revenue • Ym customers • Zm engineers
    </div>
</div>
```

### Skills Category Pattern

```html
<div class="skill-category">
    <span class="skill-label">Category Name:</span>
    <span class="skill-items">Skill 1, Skill 2, Skill 3</span>
</div>
```

## PDF Generation

**Prerequisite**: `pip install weasyprint`

```bash
# Basic usage
python scripts/generate_pdf.py resume.html

# Custom output path
python scripts/generate_pdf.py resume.html -o output/my_resume.pdf
```

The script:
- Applies A4 page size with optimized margins
- Preserves colors for print
- Reports page count (target: 2-3 pages)
- Handles natural page breaks

## Content Guidelines

### Profile Section
- 3-4 sentences summarizing expertise
- Tailor terminology to target role/industry
- Include scale indicators (team size, revenue, customers)

### Experience Bullets
- Start with `<strong>Bold headline</strong>:` pattern
- Include quantified metrics where possible
- Focus on impact and outcomes, not just responsibilities

### Scale Snapshot
- Use for most recent/significant roles only
- Include: revenue, customers, team size, key metrics
- Keep to single line

For detailed content guidance, see `references/content-guidelines.md`.

## File Locations

- **Template**: `assets/resume_template.html`
- **PDF Generator**: `scripts/generate_pdf.py`
- **Content Guide**: `references/content-guidelines.md`
