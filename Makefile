HOOKS_DIR := $(shell git rev-parse --git-common-dir)/hooks

.PHONY: install-hooks uninstall-hooks

install-hooks: ## Install git hooks
	@mkdir -p $(HOOKS_DIR)
	cp githooks/pre-push.sh $(HOOKS_DIR)/pre-push
	chmod +x $(HOOKS_DIR)/pre-push
	@echo "Installed pre-push hook to $(HOOKS_DIR)/pre-push"

uninstall-hooks: ## Remove git hooks
	rm -f $(HOOKS_DIR)/pre-push
	@echo "Removed pre-push hook"
