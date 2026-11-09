//! A deliberately finite SQL language for task-limited database capabilities.
//! Parsed input is lowered to SelectRequest and regenerated with quoted safe
//! identifiers and bound literals. The original SQL is NEVER executed here.
use serde_json::Value;
use crate::{Filter,FilterOp,SelectRequest,is_valid_identifier,MAX_FILTERS,MAX_ROWS_CAP};

#[derive(Debug,Clone,Copy,PartialEq,Eq,thiserror::Error)]
#[error("task-limited SQL accepts only a single-table SELECT with literal predicates")]
pub struct FiniteSelectError;
#[derive(Debug,Clone,PartialEq)]
enum Token {Word(String),Ident(String),Literal(Value),Comma,Star,Operator(String),Semi}

/// Grammar: SELECT (* | identifier [, identifier]*) FROM identifier
/// [WHERE identifier comparison literal [AND ...]]
/// [ORDER BY identifier [ASC|DESC]] [LIMIT positive_integer] [;].
/// Comparisons: =, !=, <>, <, <=, >, >=, LIKE, IS [NOT] NULL.
/// Comments, qualified names, aliases, functions, CTEs and nested SQL refuse.
pub fn parse_finite_select(sql:&str)->Result<SelectRequest,FiniteSelectError>{
    let tokens=lex(sql)?;let mut parser=Parser{tokens,pos:0};
    parser.require("select")?;
    let mut columns=Vec::new();
    if parser.take()==Some(Token::Star) {} else {
        parser.pos=parser.pos.checked_sub(1).ok_or(FiniteSelectError)?;
        columns.push(parser.identifier()?);
        while parser.consume(&Token::Comma) {
            if columns.len()>=128{return Err(FiniteSelectError);}
            columns.push(parser.identifier()?);
        }
    }
    parser.require("from")?;let table=parser.identifier()?;
    let mut filter=Vec::new();
    if parser.keyword("where") {
        loop {
            if filter.len()>=MAX_FILTERS{return Err(FiniteSelectError);}
            let column=parser.identifier()?;
            let (op,value)=if parser.keyword("is") {
                let op=if parser.keyword("not"){FilterOp::Ne}else{FilterOp::Eq};
                parser.require("null")?;(op,Value::Null)
            }else{
                let op=match parser.take(){Some(Token::Operator(op))=>FilterOp::parse(&op),
                    Some(Token::Word(op)) if op.eq_ignore_ascii_case("like")=>Some(FilterOp::Like),_=>None}.ok_or(FiniteSelectError)?;
                let value=match parser.take(){Some(Token::Literal(v))=>v,
                    Some(Token::Word(v)) if v.eq_ignore_ascii_case("true")=>Value::Bool(true),
                    Some(Token::Word(v)) if v.eq_ignore_ascii_case("false")=>Value::Bool(false),_=>return Err(FiniteSelectError)};
                (op,value)
            };
            filter.push(Filter{column,op,value});
            if !parser.keyword("and"){break;}
        }
    }
    let order_by=if parser.keyword("order"){
        parser.require("by")?;let column=parser.identifier()?;
        let direction=if parser.keyword("desc"){ "DESC" }else{parser.keyword("asc");"ASC"};
        Some(format!("{column} {direction}"))
    }else{None};
    let limit=if parser.keyword("limit"){
        match parser.take(){Some(Token::Literal(Value::Number(n)))=>Some(usize::try_from(n.as_u64().filter(|n|*n>0 && *n<=MAX_ROWS_CAP as u64).ok_or(FiniteSelectError)?).map_err(|_|FiniteSelectError)?),_=>return Err(FiniteSelectError)}
    }else{None};
    parser.consume(&Token::Semi);
    if parser.pos!=parser.tokens.len(){return Err(FiniteSelectError);}
    Ok(SelectRequest{table,columns,filter,order_by,limit})
}
struct Parser{tokens:Vec<Token>,pos:usize}
impl Parser{
    fn take(&mut self)->Option<Token>{let result=self.tokens.get(self.pos)?.clone();self.pos+=1;Some(result)}
    fn consume(&mut self,token:&Token)->bool{if self.tokens.get(self.pos)==Some(token){self.pos+=1;true}else{false}}
    fn keyword(&mut self,word:&str)->bool{if self.tokens.get(self.pos).is_some_and(|t|matches!(t,Token::Word(v) if v.eq_ignore_ascii_case(word))){self.pos+=1;true}else{false}}
    fn require(&mut self,word:&str)->Result<(),FiniteSelectError>{if self.keyword(word){Ok(())}else{Err(FiniteSelectError)}}
    fn identifier(&mut self)->Result<String,FiniteSelectError>{match self.take(){Some(Token::Word(s)|Token::Ident(s)) if is_valid_identifier(&s)=>Ok(s),_=>Err(FiniteSelectError)}}
}
fn lex(sql:&str)->Result<Vec<Token>,FiniteSelectError>{
    if sql.is_empty() || sql.len()>48*1024 || sql.chars().count()>crate::guard::MAX_SQL_LEN{return Err(FiniteSelectError);}
    let mut chars=sql.chars().peekable();let mut tokens=Vec::new();
    while let Some(ch)=chars.next(){
        if ch.is_ascii_whitespace(){continue;}
        let token=match ch{
            ','=>Token::Comma,'*'=>Token::Star,';'=>Token::Semi,
            '='|'!'|'<'|'>'=>{
                let mut op=ch.to_string();
                if chars.peek().is_some_and(|next|*next=='=' || ch=='<' && *next=='>'){op.push(chars.next().ok_or(FiniteSelectError)?);}
                if FilterOp::parse(&op).is_none(){return Err(FiniteSelectError);}Token::Operator(op)
            },
            '\''=>{
                let mut value=String::new();loop{
                    let ch=chars.next().ok_or(FiniteSelectError)?;
                    if ch=='\''{if chars.peek()==Some(&'\''){chars.next();value.push('\'');}else{break;}}
                    else{if ch=='\0'{return Err(FiniteSelectError);}value.push(ch);}
                }Token::Literal(Value::String(value))
            },
            '"'|'`'|'['=>{
                let end=if ch=='['{']'}else{ch};let mut ident=String::new();loop{
                    let ch=chars.next().ok_or(FiniteSelectError)?;if ch==end{break;}ident.push(ch);
                    if ident.len()>64{return Err(FiniteSelectError);}
                }
                if !is_valid_identifier(&ident){return Err(FiniteSelectError);}Token::Ident(ident)
            },
            '-'|'0'..='9'=>{
                let mut number=ch.to_string();while chars.peek().is_some_and(|c|c.is_ascii_digit() || matches!(c,'.'|'e'|'E'|'+'|'-')){number.push(chars.next().ok_or(FiniteSelectError)?);}
                let value:Value=serde_json::from_str(&number).map_err(|_|FiniteSelectError)?;
                if !value.is_number(){return Err(FiniteSelectError);}Token::Literal(value)
            },
            '_'|'a'..='z'|'A'..='Z'=>{
                let mut word=ch.to_string();while chars.peek().is_some_and(|c|c.is_ascii_alphanumeric() || *c=='_'){word.push(chars.next().ok_or(FiniteSelectError)?);}
                Token::Word(word)
            },
            _=>return Err(FiniteSelectError),
        };
        if tokens.len()>=1024{return Err(FiniteSelectError);}tokens.push(token);
    }Ok(tokens)
}

#[cfg(test)]
mod tests{
    use super::*;use serde_json::json;
    #[test]
    fn structured_select_preserves_safe_quoted_names_literal_binds_and_caps(){
        let request=parse_finite_select("SELECT \"id\", `name` FROM [customers] WHERE id >= -2 AND name = 'O''Brien; DROP TABLE secret' ORDER BY \"id\" DESC LIMIT 12;").unwrap();
        assert_eq!(request.table,"customers");assert_eq!(request.columns,vec!["id","name"]);assert_eq!(request.filter[0].value,json!(-2));
        assert_eq!(request.filter[1].value,json!("O'Brien; DROP TABLE secret"));assert_eq!(request.order_by.as_deref(),Some("id DESC"));assert_eq!(request.limit,Some(12));
        assert_eq!(parse_finite_select("SELECT * FROM customers WHERE name IS NOT NULL").unwrap().filter[0].op,FilterOp::Ne);
    }
    #[test]
    fn unsupported_sql_is_never_approximated_as_a_safe_query(){
        for sql in ["", "SELECT", "SELECT * FROM customers; SELECT * FROM secret", "SELECT * FROM customers UNION SELECT * FROM secret",
            "SELECT * FROM customers JOIN secret ON 1=1", "SELECT * FROM customers, secret", "SELECT * FROM (SELECT * FROM secret)",
            "WITH c AS (SELECT * FROM secret) SELECT * FROM c", "SELECT readfile('secret') FROM customers", "SELECT * INTO copy FROM customers",
            "SELECT * FROM customers WHERE id IN (SELECT id FROM secret)", "SELECT * FROM customers WHERE id=1 OR id=2",
            "SELECT * FROM customers WHERE id=other", "SELECT * FROM customers WHERE name=NULL", "SELECT * FROM customers --comment",
            "SELECT * FROM customers /* comment */", "SELECT * FROM schema.customers", "SELECT c.id FROM customers c", "SELECT COUNT(*) FROM customers",
            "SELECT * FROM \"secret; DROP\"", "SELECT * FROM customers LIMIT -1", "SELECT * FROM customers LIMIT 0", "SELECT * FROM customers LIMIT 1001",
            "SELECT * FROM customers LIMIT 1 OFFSET 2", "SELECT * FROM customers WHERE id=1; -- tail", "SELECT * FROM customers WHERE name=E'escape'",
            "SELECT * FROM customers WHERE name='unterminated", "SELECT * FROM customers WHERE id=:parameter", "SELECT * FROM customers WHERE id=1+2"]{
            assert!(parse_finite_select(sql).is_err(),"{sql}");
        }
    }
    #[test]
    fn literals_cannot_close_the_generated_identifier_or_add_sql(){
        for value in ["x' OR 1=1 --", "\\';SELECT * FROM secret;--", "</reference>", "你好"]{
            let escaped=value.replace('\'',"''");let request=parse_finite_select(&format!("SELECT name FROM customers WHERE name='{escaped}'")).unwrap();
            assert_eq!(request.filter[0].value,json!(value));assert_eq!(request.table,"customers");
        }
    }
    #[test]
    fn finite_sql_keeps_the_original_statement_size_limit(){
        let sql=format!("SELECT name FROM customers WHERE name='{}'","x".repeat(crate::guard::MAX_SQL_LEN));
        assert!(parse_finite_select(&sql).is_err());
    }

}
