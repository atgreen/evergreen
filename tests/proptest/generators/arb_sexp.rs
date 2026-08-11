use proptest::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sexp {
    Nil,
    Fixnum(i64),
    Character(char),
    String(String),
    Symbol(String),
    List(Vec<Sexp>),
}

fn arb_symbol_name() -> impl Strategy<Value = String> {
    "[A-Z][A-Z0-9-]{0,7}".prop_map(|name| name.to_string())
}

fn arb_string_atom() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            Just('a'),
            Just('b'),
            Just('c'),
            Just(' '),
            Just('-'),
            Just('0'),
            Just('1'),
            Just('2'),
        ],
        0..16,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

fn leaf() -> impl Strategy<Value = Sexp> {
    prop_oneof![
        Just(Sexp::Nil),
        (-1024_i64..=1024).prop_map(Sexp::Fixnum),
        prop_oneof![Just('A'), Just('Z'), Just(' '), Just('\n')].prop_map(Sexp::Character),
        arb_string_atom().prop_map(Sexp::String),
        arb_symbol_name().prop_map(Sexp::Symbol),
    ]
}

// Per R10.37 and R10.38, arb_sexp is recursive and size-bounded.
pub fn arb_sexp_with_limits(max_depth: u32, max_breadth: usize) -> BoxedStrategy<Sexp> {
    leaf()
        .prop_recursive(max_depth, 256, max_breadth as u32, move |inner| {
            proptest::collection::vec(inner, 1..=max_breadth).prop_map(Sexp::List)
        })
        .boxed()
}

pub fn arb_sexp() -> BoxedStrategy<Sexp> {
    arb_sexp_with_limits(8, 16)
}
