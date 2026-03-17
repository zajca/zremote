#!/usr/bin/env bash
# Coverage gate script - checks that test coverage stays above thresholds.
# Usage: ./scripts/check-coverage.sh [--quick]
#   --quick: skip coverage, only run tests

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

BACKEND_THRESHOLD=80
FRONTEND_THRESHOLD=75

cd "$(git rev-parse --show-toplevel)"

echo "=== Running tests ==="

echo -n "Backend tests... "
if cargo test --workspace --quiet 2>/dev/null; then
    echo -e "${GREEN}OK${NC}"
else
    echo -e "${RED}FAILED${NC}"
    exit 1
fi

echo -n "Frontend tests... "
if (cd web && bun run test 2>/dev/null | tail -1); then
    echo -e "${GREEN}OK${NC}"
else
    echo -e "${RED}FAILED${NC}"
    exit 1
fi

if [[ "${1:-}" == "--quick" ]]; then
    echo -e "\n${GREEN}All tests pass.${NC} (coverage check skipped with --quick)"
    exit 0
fi

echo ""
echo "=== Checking coverage thresholds ==="

# Backend coverage
echo -n "Backend coverage (threshold: ${BACKEND_THRESHOLD}%)... "
backend_output=$(cargo llvm-cov --workspace 2>&1 | grep "^TOTAL")
backend_pct=$(echo "$backend_output" | awk '{
    # Find the last percentage-like number before the trailing dashes
    for (i=NF; i>=1; i--) {
        if ($i ~ /^[0-9]+\.[0-9]+%$/) {
            gsub(/%/, "", $i)
            print $i
            exit
        }
    }
}')

if [ -z "$backend_pct" ]; then
    # Fallback: extract line coverage (9th field in the TOTAL row)
    backend_pct=$(echo "$backend_output" | awk '{print $10}' | tr -d '%')
fi

if (( $(echo "$backend_pct >= $BACKEND_THRESHOLD" | bc -l) )); then
    echo -e "${GREEN}${backend_pct}%${NC}"
else
    echo -e "${RED}${backend_pct}% (below ${BACKEND_THRESHOLD}%)${NC}"
    echo -e "${RED}Backend coverage regression detected!${NC}"
    exit 1
fi

# Frontend coverage
echo -n "Frontend coverage (threshold: ${FRONTEND_THRESHOLD}%)... "
frontend_output=$(cd web && bun run test:coverage 2>&1 | grep "All files")
frontend_pct=$(echo "$frontend_output" | awk -F'|' '{gsub(/[ \t]+/, "", $2); print $2}')

if [ -z "$frontend_pct" ]; then
    echo -e "${YELLOW}could not parse${NC}"
else
    if (( $(echo "$frontend_pct >= $FRONTEND_THRESHOLD" | bc -l) )); then
        echo -e "${GREEN}${frontend_pct}%${NC}"
    else
        echo -e "${RED}${frontend_pct}% (below ${FRONTEND_THRESHOLD}%)${NC}"
        echo -e "${RED}Frontend coverage regression detected!${NC}"
        exit 1
    fi
fi

echo ""
echo -e "${GREEN}Coverage gates passed.${NC}"
