import os

# go-task colours its output when CI=true (every CI runner), gluing an ANSI code to the
# lines these suites match whole. NO_COLOR turns that off; FORCE_COLOR would beat it.
os.environ.pop("FORCE_COLOR", None)
os.environ["NO_COLOR"] = "1"
