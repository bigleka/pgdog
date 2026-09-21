use crate::{
    config::PreparedStatements,
    frontend::router::parser::{AstContext, Cache},
};

use super::*;

impl QueryEngine {
    /// Rewrite extended protocol messages.
    pub(super) fn rewrite_extended(
        &mut self,
        context: &mut QueryEngineContext<'_>,
    ) -> Result<(), Error> {
        for message in context.client_request.iter_mut() {
            if message.extended() {
                let level = context.prepared_statements.level;
                match (level, message.anonymous()) {
                    (PreparedStatements::ExtendedAnonymous, _)
                    | (PreparedStatements::Extended, false) => {
                        context.prepared_statements.maybe_rewrite(message)?
                    }
                    _ => (),
                }
            }
        }
        Ok(())
    }

    /// Parse client request and rewrite it, if necessary.
    pub(super) async fn parse_and_rewrite(
        &mut self,
        context: &mut QueryEngineContext<'_>,
    ) -> Result<bool, Error> {
        let mut use_parser = self
            .backend
            .cluster()
            .map(|cluster| cluster.use_query_parser(context.client_request))
            .unwrap_or(false);

        // Force parser for SELECTs and DMLs (INSERT, UPDATE, DELETE) if result cache is enabled,
        // so we can identify tables for caching and table-based invalidation.
        if !use_parser && self.result_cache.is_some() {
            if let Ok(Some(q)) = context.client_request.query() {
                let trimmed = q.query().trim_start();
                let first_word = trimmed.split_whitespace().next().unwrap_or_default();
                if first_word.eq_ignore_ascii_case("select")
                    || first_word.eq_ignore_ascii_case("insert")
                    || first_word.eq_ignore_ascii_case("update")
                    || first_word.eq_ignore_ascii_case("delete")
                {
                    use_parser = true;
                }
            }
        }

        if !use_parser {
            return Ok(true);
        }

        let query = context.client_request.query()?;
        if let Some(query) = query {
            let cluster = self.backend.cluster()?;
            let ast_ctx = AstContext::from_cluster(cluster, context.params);
            let ast = match Cache::get().query(&query, &ast_ctx, context.prepared_statements) {
                Ok(ast) => ast,
                Err(err) => {
                    self.error_response(context, ErrorResponse::syntax(err.to_string().as_str()))
                        .await?;
                    return Ok(false);
                }
            };
            context.client_request.ast = Some(ast);
        }

        let plan = context
            .client_request
            .ast
            .as_ref()
            .map(|ast| ast.rewrite_plan.clone());

        if let Some(plan) = plan {
            context.rewrite_result = Some(plan.apply(context.client_request)?);
        }

        Ok(true)
    }
}
