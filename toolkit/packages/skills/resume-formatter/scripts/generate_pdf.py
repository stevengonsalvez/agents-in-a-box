#!/usr/bin/env python3
"""
Resume PDF Generator
Converts HTML resume to PDF using WeasyPrint with optimized settings
"""

import weasyprint
from weasyprint import HTML, CSS
import sys
import os
import argparse


def generate_pdf(html_file, output_file=None):
    """
    Generate PDF from HTML resume with professional formatting

    Args:
        html_file: Path to HTML resume file
        output_file: Path for output PDF (defaults to same name as HTML with .pdf extension)

    Returns:
        bool: True if successful, False otherwise
    """
    if not os.path.exists(html_file):
        print(f"Error: {html_file} not found!")
        return False

    if output_file is None:
        output_file = os.path.splitext(html_file)[0] + '.pdf'

    try:
        # PDF optimization CSS
        pdf_css = CSS(string='''
            @page {
                size: A4;
                margin: 0.5in 0.6in;
            }

            body {
                -webkit-print-color-adjust: exact;
                print-color-adjust: exact;
            }

            /* Natural page flow */
            * {
                page-break-before: auto !important;
                page-break-after: auto !important;
                page-break-inside: auto !important;
            }

            /* Keep job title with first bullet */
            .job-header {
                page-break-after: avoid;
            }

            /* Allow sections to flow naturally */
            .role-card, .section, .section-header, .scale-snapshot {
                page-break-inside: auto;
                page-break-before: auto;
                page-break-after: auto;
            }

            .container {
                padding: 0;
            }

            .section-header, .scale-snapshot {
                -webkit-print-color-adjust: exact;
                print-color-adjust: exact;
            }
        ''')

        print(f"Loading {html_file}...")
        html = HTML(filename=html_file)

        print(f"Generating PDF: {output_file}...")
        html.write_pdf(
            output_file,
            stylesheets=[pdf_css],
            presentational_hints=True
        )

        file_size = os.path.getsize(output_file) / 1024
        print(f"PDF generated successfully!")
        print(f"   File: {output_file}")
        print(f"   Size: {file_size:.1f} KB")

        document = html.render(stylesheets=[pdf_css])
        page_count = len(document.pages)
        print(f"   Pages: {page_count}")

        if page_count > 3:
            print(f"   Warning: Resume exceeds 3 pages ({page_count} pages)")
        else:
            print(f"   Fits within target of 2-3 pages")

        return True

    except Exception as e:
        print(f"Error generating PDF: {str(e)}")
        return False


def main():
    parser = argparse.ArgumentParser(description='Generate PDF from HTML resume')
    parser.add_argument('html_file', help='Path to HTML resume file')
    parser.add_argument('-o', '--output', help='Output PDF file path (default: same as input with .pdf extension)')

    args = parser.parse_args()

    success = generate_pdf(args.html_file, args.output)

    if success:
        print("\nResume PDF ready!")
    else:
        print("\nPDF generation failed.")
        sys.exit(1)


if __name__ == "__main__":
    main()
