# make paper: reproduce every number, figure and design constant of THOR 2 paper A (moeyensj/thor_paper2), then
# compile the paper and check it.
#
#   make paper PAPER=~/path/to/thor_paper2 PYTHON=<a python with matplotlib, pyarrow and the figure style>
#   make -B paper ...     re-run every experiment, whatever changed (do this in a fresh clone: git does not keep
#                         modification times, so make cannot tell there what changed)
#
# Each experiment <x> has src/bin/<x>.rs, its command in run/<x>.sh and its inputs in inputs/<x>.sha256. Its record
# results/<x>.json depends on its code, THOR's pin (thor.rev) and its inputs, so make re-runs exactly the experiments
# whose code, pin or inputs changed. Every input's SHA-256 is checked before a run: a missing or changed input stops
# the build. Inputs are this machine's paths (thor-rust-experiments, /var/tmp/jul_hp128).
#
# RUN prefixes every experiment and is where the house rules go, for example:
#   RUN="systemd-run --user --scope -p MemoryMax=40G -p MemorySwapMax=0 nice -n 5 taskset -c 44-59"

PAPER ?= $(error set PAPER to a checkout of moeyensj/thor_paper2)
MAIN ?= sample701
THOR_CLONE ?= $(HOME)/projects/thor/thor-rust
PYTHON ?= python3
RUN ?=
MEASURED := clustering_speed
export CARGO_TARGET_DIR ?= $(HOME)/projects/thor/wt/paper2-build/target
TEXBUILD ?= $(CARGO_TARGET_DIR)/tex-make-paper
BIN := $(CARGO_TARGET_DIR)/release

.PHONY: paper engine build experiments figures pdf check
paper: check

engine: vendor/thor-rs/.thor-rev
vendor/thor-rs/.thor-rev: thor.rev
	./fetch_thor.sh $(THOR_CLONE)

# One build for every experiment; fig_gather propagates with ASSIST.
build: engine
	cargo build --release --features assist --bins

RESULTS := results/clustering_speed.json results/fig_radius_solve.json results/constants.json \
           results/fig_gather/fig_gather.json
experiments: $(RESULTS)

define experiment
	sha256sum --quiet --strict -c inputs/$(1).sha256
	PAPER=$(PAPER) BIN=$(BIN) $(RUN) bash run/$(1).sh
endef

results/%.json: src/bin/%.rs thor.rev inputs/%.sha256 run/%.sh | build
	$(call experiment,$*)

results/fig_gather/fig_gather.json: src/bin/fig_gather.rs thor.rev inputs/fig_gather.sha256 run/fig_gather.sh | build
	$(call experiment,fig_gather)

figures: $(PAPER)/generated/fig_gather.pdf $(PAPER)/generated/fig_radius_solve.pdf
$(PAPER)/generated/fig_gather.pdf: figures/fig_gather.py results/fig_gather/fig_gather.json
	$(PYTHON) figures/fig_gather.py $(PAPER)
$(PAPER)/generated/fig_radius_solve.pdf: figures/fig_radius_solve.py results/fig_radius_solve.json
	$(PYTHON) figures/fig_radius_solve.py $(PAPER)

pdf: experiments figures
	rm -rf $(TEXBUILD) && mkdir -p $(TEXBUILD) && cp -r $(PAPER)/. $(TEXBUILD)/
	cd $(TEXBUILD) && pdflatex -interaction=nonstopmode -halt-on-error $(MAIN).tex >/dev/null \
	  && bibtex $(MAIN) >/dev/null && pdflatex -interaction=nonstopmode -halt-on-error $(MAIN).tex >/dev/null \
	  && pdflatex -interaction=nonstopmode -halt-on-error $(MAIN).tex >/dev/null
	! grep -E '^!|There were undefined (references|citations)|(Reference|Citation) .* undefined' $(TEXBUILD)/$(MAIN).log
	@echo "compiled: $(TEXBUILD)/$(MAIN).pdf"

check: pdf
	python3 tools/check_numbers.py $(PAPER)/$(MAIN).tex --allow $(PAPER)/numbers.allow
	python3 tools/compare_generated.py $(PAPER) --measured $(MEASURED)
