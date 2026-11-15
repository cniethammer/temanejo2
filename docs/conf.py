# Sphinx configuration for the Temanejo documentation.
# Build: `make -C docs html` (HTML) and `make -C docs man` (man pages).

project = "Temanejo"
author = "Christoph Niethammer"
copyright = "2026, Christoph Niethammer"
version = release = "0.1.0"

extensions = []
# Virtual environments inside docs/ contain .rst files of installed packages.
exclude_patterns = ["_build", ".venv", "venv", ".*"]
highlight_language = "none"
# Keep "--option" intact; smart quotes would turn it into an en dash.
smartquotes = False

html_theme = "alabaster"
html_title = "Temanejo 0.1"

# (source file, name, description, authors, section)
man_pages = [
    ("man/temanejo", "temanejo", "task graph analysis server and browser GUI", [author], 1),
]
man_show_urls = True
