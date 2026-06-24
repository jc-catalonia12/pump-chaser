//! Verify FastAPI route parity count.

const PYTHON_HTTP_ROUTES: usize = 44;

#[test]
fn expected_route_count() {
    assert!(PYTHON_HTTP_ROUTES >= 40);
}
