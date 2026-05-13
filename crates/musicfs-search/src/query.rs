use tantivy::query::{BooleanQuery, FuzzyTermQuery, Occur, Query};
use tantivy::schema::Field;
use tantivy::Term;

pub struct SearchQueryBuilder {
    fields: Vec<Field>,
    default_fuzziness: u8,
}

impl SearchQueryBuilder {
    pub fn new(fields: Vec<Field>) -> Self {
        Self {
            fields,
            default_fuzziness: 1,
        }
    }

    pub fn with_fuzziness(mut self, fuzziness: u8) -> Self {
        self.default_fuzziness = fuzziness;
        self
    }

    pub fn build_fuzzy(&self, query_text: &str) -> Box<dyn Query> {
        let terms: Vec<_> = query_text
            .split_whitespace()
            .filter(|t| !t.is_empty())
            .collect();

        if terms.is_empty() {
            return Box::new(tantivy::query::AllQuery);
        }

        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();

        for term in terms {
            let mut field_queries: Vec<(Occur, Box<dyn Query>)> = Vec::new();

            for field in &self.fields {
                let fuzzy = FuzzyTermQuery::new(
                    Term::from_field_text(*field, &term.to_lowercase()),
                    self.default_fuzziness,
                    true,
                );
                field_queries.push((Occur::Should, Box::new(fuzzy)));
            }

            let field_union = BooleanQuery::new(field_queries);
            clauses.push((Occur::Must, Box::new(field_union)));
        }

        Box::new(BooleanQuery::new(clauses))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tantivy::schema::{Schema, TEXT};

    #[test]
    fn test_query_builder() {
        let mut schema_builder = Schema::builder();
        let artist = schema_builder.add_text_field("artist", TEXT);
        let title = schema_builder.add_text_field("title", TEXT);

        let builder = SearchQueryBuilder::new(vec![artist, title]);
        let _query = builder.build_fuzzy("metallica sandman");
    }

    #[test]
    fn test_empty_query() {
        let mut schema_builder = Schema::builder();
        let artist = schema_builder.add_text_field("artist", TEXT);

        let builder = SearchQueryBuilder::new(vec![artist]);
        let _query = builder.build_fuzzy("");
    }
}
