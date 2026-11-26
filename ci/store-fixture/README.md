# Test store catalog (for ci/e2e.sh)

Two small entries and their built catalog in `dist/`, signed with a throwaway test key
(public key in `ci/e2e.sh`; never trusted by real Hearth builds). Rebuild after changing an entry:

    hearth-store build ci/store-fixture --out ci/store-fixture/dist
    HEARTH_STORE_KEY="$(cat test.key)" HEARTH_STORE_KEY_PASSWORD=test hearth-store sign ci/store-fixture/dist/index.json
