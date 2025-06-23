# Delver Documentation Site

This directory contains the complete Jekyll-powered documentation site for Delver. The site is designed to be compatible with GitHub Pages and provides comprehensive documentation for users and developers.

## Site Structure

```
docs/
├── _config.yml              # Jekyll configuration
├── Gemfile                  # Jekyll dependencies
├── index.md                 # Homepage
├── getting-started.md       # Quick start guide
├── documentation.md         # Documentation overview
├── api.md                   # API reference
├── _layouts/                # Page layouts
│   ├── default.html         # Base layout
│   ├── page.html            # Standard page layout
│   └── doc.html             # Documentation layout
├── _includes/               # Reusable components
│   ├── head.html            # HTML head section
│   ├── header.html          # Site header/navigation
│   ├── footer.html          # Site footer
│   └── social.html          # Social media links
├── _docs/                   # Documentation collection
│   ├── template-syntax.md   # Template language guide
│   ├── parser.md            # Parser technical details
│   ├── collation.md         # Content collation algorithms
│   └── implementation-plan.md # System architecture
└── assets/
    └── css/
        └── main.scss        # Site styling
```

## Local Development

### Prerequisites

- Ruby 2.7+ and Bundler
- Jekyll and GitHub Pages gems

### Setup

1. Install dependencies:
   ```bash
   cd docs
   bundle install
   ```

2. Serve the site locally:
   ```bash
   bundle exec jekyll serve
   ```

3. View at http://localhost:4000

### GitHub Pages Deployment

The site is configured for automatic deployment on GitHub Pages:

1. Push changes to the main branch
2. GitHub Actions will build and deploy the site
3. Site will be available at `https://yourusername.github.io/delver/`

## Content Organization

### Pages vs Collections

- **Pages** (`*.md` in root): Standalone pages like homepage, getting started, API reference
- **Collections** (`_docs/`): Related documentation that benefits from automatic navigation and cross-linking

### Front Matter

All content files use Jekyll front matter for metadata:

```yaml
---
layout: doc                    # Layout to use
title: Page Title             # Page title
description: "Page description" # Meta description
toc: true                     # Enable table of contents
tags: [tag1, tag2]           # Content tags
---
```

### Layout Types

- **default**: Base layout with header/footer
- **page**: Standard page with optional TOC
- **doc**: Documentation layout with sidebar navigation

## Styling

The site uses a modern, responsive design with:

- CSS custom properties for theming
- Dark mode support via `prefers-color-scheme`
- Mobile-responsive grid layouts
- Syntax highlighting for code blocks
- Professional typography with Inter and JetBrains Mono fonts

### Color Scheme

- Primary: Blue (#2563eb)
- Secondary: Slate gray (#64748b)
- Backgrounds: White/gray scale with dark mode variants
- Syntax highlighting: Tomorrow theme

## Navigation

Navigation is configured in `_config.yml`:

```yaml
navigation:
  - title: Home
    url: /
  - title: Documentation
    url: /documentation/
    submenu:
      - title: Template Syntax
        url: /docs/template-syntax/
      # ... more items
```

## Features

### Hero Section
Eye-catching landing area with gradient background and call-to-action buttons.

### Feature Grid
Responsive grid layout for showcasing key features with hover effects.

### Documentation Layout
Two-column layout with sticky sidebar navigation for easy browsing.

### Code Highlighting
Syntax highlighting for Rust, Python, TOML, and other languages using Prism.js.

### SEO Optimization
- Meta tags and structured data
- Sitemap generation
- Social media integration
- Performance optimization

## Content Guidelines

### Writing Style
- Clear, concise technical writing
- Code examples for all concepts
- Progressive disclosure (basic → advanced)
- Cross-references between related topics

### Code Examples
- Always include working code snippets
- Show both Rust and Python versions when applicable
- Include expected output where relevant
- Use meaningful variable names and comments

### Documentation Structure
1. Brief overview/introduction
2. Core concepts and terminology
3. Step-by-step examples
4. Advanced usage patterns
5. Troubleshooting and common issues
6. Related topics and next steps

## Contributing

### Adding New Documentation

1. Create new file in appropriate directory (`_docs/` for technical docs)
2. Add proper front matter with layout, title, description
3. Update navigation in `_config.yml` if needed
4. Cross-link from related pages
5. Test locally before committing

### Updating Existing Content

1. Make changes to markdown files
2. Verify links and references still work
3. Update any affected navigation or cross-references
4. Test locally to ensure styling works correctly

### Style Guidelines

- Use consistent heading hierarchy (H1 for page title, H2 for major sections)
- Include code language hints for syntax highlighting
- Use tables for parameter documentation
- Include "What's Next" sections for navigation
- Add descriptive alt text for any images

## Troubleshooting

### Common Issues

**Bundle install fails**
- Ensure Ruby 2.7+ is installed
- Try `bundle update` if dependencies conflict

**Site doesn't render correctly**
- Check for YAML syntax errors in front matter
- Verify all includes and layouts exist
- Clear Jekyll cache: `bundle exec jekyll clean`

**GitHub Pages deployment fails**
- Check GitHub Actions logs for specific errors
- Ensure all files use GitHub Pages compatible features
- Verify `_config.yml` syntax is correct

### Getting Help

- Jekyll documentation: https://jekyllrb.com/docs/
- GitHub Pages docs: https://docs.github.com/en/pages
- Liquid templating: https://shopify.github.io/liquid/

## Future Enhancements

Planned improvements for the documentation site:

- **Interactive Examples**: Embedded code editors for trying templates
- **Search Functionality**: Full-text search across all documentation
- **Version Selector**: Support for multiple Delver versions
- **API Explorer**: Interactive API documentation with live examples
- **Community Features**: User contributions and example templates
- **Performance Analytics**: Track documentation usage and effectiveness

---

This documentation site provides a solid foundation for Delver's public-facing documentation, combining professional design with comprehensive technical content to help users get the most out of the platform.