#[macro_export]
macro_rules! boxed {
    ($val:expr) => {
        Box::new($val.into())
    };
}

#[macro_export]
macro_rules! node {
    ($struct:path { $($field:ident : $val:expr),* $(,)? }) => {{
        $struct {
            $(
                $field: $val,
            )*
            range: TextRange::default(),
            node_index: ast::AtomicNodeIndex::default(),
        }
    }};
}
