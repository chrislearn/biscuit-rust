//! Verifier structure and associated functions
use super::builder::{
    constrained_rule, date, fact, pred, s, string, Fact,
    Rule, Check, var, Expression, Op, Binary, Term, Policy,
    PolicyKind, Unary,
};
use super::Biscuit;
use crate::datalog;
use crate::error;
use std::{convert::TryInto, time::{SystemTime, Duration}, default::Default};
use crate::time::Instant;

/// used to check authorization policies on a token
///
/// can be created from [`Biscuit::verify`](`crate::token::Biscuit::verify`) or [`Verifier::new`]
#[derive(Clone)]
pub struct Verifier {
    world: datalog::World,
    symbols: datalog::SymbolTable,
    checks: Vec<Check>,
    token_checks: Vec<Vec<datalog::Check>>,
    policies: Vec<Policy>,
    has_token: bool,
}

impl Verifier {
    pub(crate) fn from_token(token: &Biscuit) -> Result<Self, error::Logic> {
        let world = token.generate_world(&token.symbols)?;
        let symbols = token.symbols.clone();

        Ok(Verifier {
            world,
            symbols,
            checks: vec![],
            token_checks: token.checks(),
            policies: vec![],
            has_token: true,
        })
    }

    /// creates a new empty verifier
    ///
    /// this can be used to check policies when:
    /// * there is no token (unauthenticated case)
    /// * there is a lot of data to load in the verifier on each check
    ///
    /// In the latter case, we can create an empty verifier, load it
    /// with the facts, rules and checks, and each time a token must be checked,
    /// clone the verifier and load the token with [`Verifier::add_token`]
    pub fn new() -> Result<Self, error::Logic> {
        let world = datalog::World::new();
        let symbols = super::default_symbol_table();

        Ok(Verifier {
            world,
            symbols,
            checks: vec![],
            token_checks: vec![],
            policies: vec![],
            has_token: false,
        })
    }

    /// Loads a token's facts, rules and checks in a verifier
    pub fn add_token(&mut self, token: &Biscuit) -> Result<(), error::Logic> {
        if self.has_token {
            return Err(error::Logic::VerifierNotEmpty);
        } else {
            self.has_token = true;
        }

        let authority_index = self.symbols.get("authority").unwrap();
        let ambient_index = self.symbols.get("ambient").unwrap();

        for fact in token.authority.facts.iter().cloned() {
            if fact.predicate.ids[0] == datalog::ID::Symbol(ambient_index) {
                return Err(error::Logic::InvalidAuthorityFact(
                    token.symbols.print_fact(&fact),
                ));
            }

            let fact = Fact::convert_from(&fact, &token.symbols).convert(&mut self.symbols);
            self.world.facts.insert(fact);
        }

        for rule in token.authority.rules.iter().cloned() {
            let rule = Rule::convert_from(&rule, &token.symbols).convert(&mut self.symbols);
            self.world.rules.push(rule);
        }

        for (i, block) in token.blocks.iter().enumerate() {
            // blocks cannot provide authority or ambient facts
            for fact in block.facts.iter().cloned() {
                if fact.predicate.ids[0] == datalog::ID::Symbol(authority_index)
                    || fact.predicate.ids[0] == datalog::ID::Symbol(ambient_index)
                {
                    return Err(error::Logic::InvalidBlockFact(
                        i as u32,
                        token.symbols.print_fact(&fact),
                    ));
                }

                let fact = Fact::convert_from(&fact, &token.symbols).convert(&mut self.symbols);
                self.world.facts.insert(fact);
            }

            for rule in block.rules.iter().cloned() {
                // block rules cannot generate authority or ambient facts
                if rule.head.ids[0] == datalog::ID::Symbol(authority_index)
                    || rule.head.ids[0] == datalog::ID::Symbol(ambient_index)
                {
                    return Err(error::Logic::InvalidBlockRule(
                        i as u32,
                        token.symbols.print_rule(&rule),
                    ));
                }

                let rule = Rule::convert_from(&rule, &token.symbols).convert(&mut self.symbols);
                self.world.rules.push(rule);
            }
        }

        let mut token_checks: Vec<Vec<datalog::Check>> = Vec::new();
        let checks = token.authority.checks.iter()
            .map(|c| Check::convert_from(&c, &token.symbols).convert(&mut self.symbols))
            .collect();
        token_checks.push(checks);

        for block in token.blocks.iter() {
            let checks = block.checks.iter()
                .map(|c| Check::convert_from(&c, &token.symbols).convert(&mut self.symbols))
                .collect();
            token_checks.push(checks);
        }

        self.token_checks = token_checks;
        Ok(())
    }

    /// add a fact to the verifier
    pub fn add_fact<F: TryInto<Fact>>(&mut self, fact: F) -> Result<(), error::Token> {
        let fact = fact.try_into().map_err(|_| error::Token::ParseError)?;
        self.world.facts.insert(fact.convert(&mut self.symbols));
        Ok(())
    }

    /// add a rule to the verifier
    pub fn add_rule<R: TryInto<Rule>>(&mut self, rule: R) -> Result<(), error::Token> {
        let rule = rule.try_into().map_err(|_| error::Token::ParseError)?;
        self.world.rules.push(rule.convert(&mut self.symbols));
        Ok(())
    }

    /// run a query over the verifier's Datalog engine to gather data
    pub fn query<R: TryInto<Rule>>(
        &mut self,
        rule: R,
    ) -> Result<Vec<Fact>, error::Token> {
        self.query_with_limits(rule, VerifierLimits::default())
    }

    /// run a query over the verifier's Datalog engine to gather data
    ///
    /// this method can specify custom runtime limits
    pub fn query_with_limits<R: TryInto<Rule>>(
        &mut self,
        rule: R,
        limits: VerifierLimits
    ) -> Result<Vec<Fact>, error::Token> {
        let rule = rule.try_into().map_err(|_| error::Token::ParseError)?;
        self.world.run_with_limits(limits.into()).map_err(error::Token::RunLimit)?;
        let mut res = self.world.query_rule(rule.convert(&mut self.symbols));

        Ok(res
           .drain(..)
           .map(|f| Fact::convert_from(&f, &self.symbols))
           .collect())
    }

    /// add a check to the verifier
    pub fn add_check<R: TryInto<Check>>(&mut self, check: R) -> Result<(), error::Token> {
        let check = check.try_into().map_err(|_| error::Token::ParseError)?;
        self.checks.push(check);
        Ok(())
    }

    pub fn add_resource(&mut self, resource: &str) {
        let fact = fact("resource", &[s("ambient"), string(resource)]);
        self.world.facts.insert(fact.convert(&mut self.symbols));
    }

    pub fn add_operation(&mut self, operation: &str) {
        let fact = fact("operation", &[s("ambient"), s(operation)]);
        self.world.facts.insert(fact.convert(&mut self.symbols));
    }

    /// adds a fact with the current time
    pub fn set_time(&mut self) {
        let fact = fact("time", &[s("ambient"), date(&SystemTime::now())]);
        self.world.facts.insert(fact.convert(&mut self.symbols));
    }

    pub fn revocation_check(&mut self, ids: &[i64]) {
        let check = constrained_rule(
            "revocation_check",
            &[var("id")],
            &[pred("revocation_id", &[var("id")])],
            &[Expression {
                ops: vec![
                    Op::Value(Term::Set(ids.iter().map(|i| Term::Integer(*i)).collect())),
                    Op::Value(var("id")),
                    Op::Binary(Binary::Contains),
                    Op::Unary(Unary::Negate),
                ]
            }],
        );
        let _ = self.add_check(check);
    }

    /// add a policy to the verifier
    pub fn add_policy<R: TryInto<Policy>>(&mut self, policy: R) -> Result<(), error::Token> {
        let policy = policy.try_into().map_err(|_| error::Token::ParseError)?;
        self.policies.push(policy);
        Ok(())
    }

    pub fn allow(&mut self) -> Result<(), error::Token> {
        self.add_policy("allow if true")
    }

    pub fn deny(&mut self) -> Result<(), error::Token> {
        self.add_policy("deny if true")
    }

    /// checks all the checks
    ///
    /// on error, this can return a list of all the failed checks
    pub fn verify(&mut self) -> Result<(), error::Token> {
        self.verify_with_limits(VerifierLimits::default())
    }

    /// checks all the checks
    ///
    /// on error, this can return a list of all the failed checks
    ///
    /// this method can specify custom runtime limits
    pub fn verify_with_limits(&mut self, limits: VerifierLimits) -> Result<(), error::Token> {
        let start = Instant::now();

        //FIXME: should check for the presence of any other symbol in the token
        if self.symbols.get("authority").is_none() || self.symbols.get("ambient").is_none() {
            return Err(error::Token::MissingSymbols);
        }

        self.world.run_with_limits(limits.clone().into()).map_err(error::Token::RunLimit)?;

        let time_limit = start + limits.max_time;

        let mut errors = vec![];
        for (i, check) in self.checks.iter().enumerate() {
            let c = check.convert(&mut self.symbols);
            let mut successful = false;

            for query in check.queries.iter() {
                let res = self.world.query_rule(query.convert(&mut self.symbols));

                let now = Instant::now();
                if now >= time_limit {
                    return Err(error::Token::RunLimit(error::RunLimit::Timeout));
                }

                if !res.is_empty() {
                    successful = true;
                    break;
                }
            }

            if !successful {
                errors.push(error::FailedCheck::Verifier(error::FailedVerifierCheck {
                    check_id: i as u32,
                    rule: self.symbols.print_check(&c),
                }));
            }
        }

        for (i, block_checks) in self.token_checks.iter().enumerate() {
            for (j, check) in block_checks.iter().enumerate() {
                let mut successful = false;

                for query in check.queries.iter() {
                    let res = self.world.query_rule(query.clone());

                    let now = Instant::now();
                    if now >= time_limit {
                        return Err(error::Token::RunLimit(error::RunLimit::Timeout));
                    }

                    if !res.is_empty() {
                        successful = true;
                        break;
                    }
                }

                if !successful {
                    errors.push(error::FailedCheck::Block(error::FailedBlockCheck {
                        block_id: i as u32,
                        check_id: j as u32,
                        rule: self.symbols.print_check(check),
                    }));
                }
            }
        }

        if !errors.is_empty() {
            Err(error::Token::FailedLogic(error::Logic::FailedChecks(
                errors,
            )))
        } else {
            for (_i, policy) in self.policies.iter().enumerate() {

                for query in policy.queries.iter() {
                    let res = self.world.query_match(query.convert(&mut self.symbols));

                    let now = Instant::now();
                    if now >= time_limit {
                        return Err(error::Token::RunLimit(error::RunLimit::Timeout));
                    }

                    if res {
                        return match policy.kind {
                            PolicyKind::Allow => Ok(()),
                            PolicyKind::Deny =>
                                Err(error::Token::FailedLogic(error::Logic::Deny))
                        }
                    }
                }
            }
            Err(error::Token::FailedLogic(error::Logic::NoMatchingPolicy))
        }
    }

    /// prints the content of the verifier
    pub fn print_world(&self) -> String {
        let mut facts = self.world
            .facts
            .iter()
            .map(|f| self.symbols.print_fact(f))
            .collect::<Vec<_>>();
        facts.sort();

        let mut rules = self.world
            .rules
            .iter()
            .map(|r| self.symbols.print_rule(r))
            .collect::<Vec<_>>();
        rules.sort();

        let mut checks = Vec::new();
        for (index, check) in self.checks.iter().enumerate() {
            checks.push(format!("Verifier[{}]: {}", index, check));
        }

        for (i, block_checks) in self.token_checks.iter().enumerate() {
            for (j, check) in block_checks.iter().enumerate() {
                checks.push(format!("Block[{}][{}]: {}", i, j, self.symbols.print_check(check)));
            }
        }

        let mut policies = Vec::new();
        for policy in self.policies.iter() {
            policies.push(policy.to_string());
        }

        format!("World {{\n  facts: {:#?}\n  rules: {:#?}\n  checks: {:#?}\n  policies: {:#?}\n}}", facts, rules, checks, policies)
    }

    /// returns all of the data loaded in the verifier
    pub fn dump(&self) -> (Vec<Fact>, Vec<Rule>, Vec<Check>) {
        let mut checks = self.checks.clone();
        checks.extend(self.token_checks.iter().flatten().map(|c| Check::convert_from(c, &self.symbols)));

        (self.world.facts.iter().map(|f| Fact::convert_from(f, &self.symbols)).collect(),
         self.world.rules.iter().map(|r| Rule::convert_from(r, &self.symbols)).collect(),
         checks
        )
    }
}

/// runtime limits for the Datalog engine
#[derive(Debug,Clone)]
pub struct VerifierLimits {
    /// maximum number of Datalog facts (memory usage)
    pub max_facts: u32,
    /// maximum number of iterations of the rules applications (prevents degenerate rules)
    pub max_iterations: u32,
    /// maximum execution time
    pub max_time: Duration,
}

impl Default for VerifierLimits {
    fn default() -> Self {
        VerifierLimits {
            max_facts: 1000,
            max_iterations: 100,
            max_time: Duration::from_millis(1),
        }
    }
}

impl std::convert::From<VerifierLimits> for crate::datalog::RunLimits {
    fn from(limits: VerifierLimits) -> Self {
        crate::datalog::RunLimits {
            max_facts: limits.max_facts,
            max_iterations: limits.max_iterations,
            max_time: limits.max_time,
        }

    }
}
