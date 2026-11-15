# Minimal Sphinx makefile: `make html`, `make man`, `make clean`.
SPHINXBUILD ?= sphinx-build
SPHINXOPTS  ?= -W --keep-going
BUILDDIR    = _build

html man:
	$(SPHINXBUILD) -M $@ . $(BUILDDIR) $(SPHINXOPTS)

clean:
	rm -rf $(BUILDDIR)

.PHONY: html man clean
