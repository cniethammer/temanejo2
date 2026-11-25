# Builds and installs temanejo, the OMPT tool library and the documentation.
#
#   make                                  # cargo build --release + docs (HTML, man)
#   make install PREFIX=/opt/temanejo     # default PREFIX: /usr/local
#   make install DESTDIR=/tmp/stage       # staged install (packaging)
#   make install-nodocs                   # without Sphinx
#   make uninstall PREFIX=/opt/temanejo
#
# Installed files:
#   $(BINDIR)/temanejo               analysis server with the GUI built in
#   $(BINDIR)/tmj-run                launcher wrapper, pointing to $(LIBDIR)
#   $(LIBDIR)/libtmj_ompt.so         OMPT tool (OMP_TOOL_LIBRARIES)
#   $(MANDIR)/man1/temanejo.1        man page
#   $(DOCDIR)/html/                  HTML documentation
#   $(DOCDIR)/LICENSE, README.md

PREFIX  ?= /usr/local
BINDIR  ?= $(PREFIX)/bin
LIBDIR  ?= $(PREFIX)/lib
DATADIR ?= $(PREFIX)/share
MANDIR  ?= $(DATADIR)/man
DOCDIR  ?= $(DATADIR)/doc/temanejo
DESTDIR ?=

CARGO   ?= cargo
INSTALL ?= install

# Use the virtual environment in docs/ if there is one.
SPHINXBUILD ?= $(if $(wildcard docs/.venv/bin/sphinx-build),$(CURDIR)/docs/.venv/bin/sphinx-build,sphinx-build)

RELEASE = target/release
LIB     = libtmj_ompt.so

all: build docs

build:
	$(CARGO) build --release

docs:
	$(MAKE) -C docs html man SPHINXBUILD=$(SPHINXBUILD)

check:
	$(CARGO) test --workspace

install: install-bin install-lib install-docs

install-nodocs: install-bin install-lib

install-bin: build
	$(INSTALL) -d $(DESTDIR)$(BINDIR)
	$(INSTALL) -m 755 $(RELEASE)/temanejo $(DESTDIR)$(BINDIR)/temanejo
	sed 's|^lib=.*|lib=$${TEMANEJO_LIB:-$(LIBDIR)/$(LIB)}|' scripts/tmj-run > $(DESTDIR)$(BINDIR)/tmj-run
	grep -q '^lib=.*$(LIBDIR)/$(LIB)' $(DESTDIR)$(BINDIR)/tmj-run
	chmod 755 $(DESTDIR)$(BINDIR)/tmj-run

install-lib: build
	$(INSTALL) -d $(DESTDIR)$(LIBDIR)
	$(INSTALL) -m 755 $(RELEASE)/$(LIB) $(DESTDIR)$(LIBDIR)/$(LIB)

install-docs: docs
	$(INSTALL) -d $(DESTDIR)$(MANDIR)/man1 $(DESTDIR)$(DOCDIR)
	$(INSTALL) -m 644 docs/_build/man/temanejo.1 $(DESTDIR)$(MANDIR)/man1/temanejo.1
	$(INSTALL) -m 644 LICENSE README.md $(DESTDIR)$(DOCDIR)/
	rm -rf $(DESTDIR)$(DOCDIR)/html
	cp -R docs/_build/html $(DESTDIR)$(DOCDIR)/html
	rm -rf $(DESTDIR)$(DOCDIR)/html/.buildinfo $(DESTDIR)$(DOCDIR)/html/_sources

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/temanejo $(DESTDIR)$(BINDIR)/tmj-run
	rm -f $(DESTDIR)$(LIBDIR)/$(LIB)
	rm -f $(DESTDIR)$(MANDIR)/man1/temanejo.1
	rm -rf $(DESTDIR)$(DOCDIR)

clean:
	$(CARGO) clean
	$(MAKE) -C docs clean

.PHONY: all build docs check install install-nodocs install-bin install-lib \
        install-docs uninstall clean
