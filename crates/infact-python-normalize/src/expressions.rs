//! Python expressions, lowered to `Form`.
//!
//! The statement half stays in `lib.rs`. The two halves meet at a narrow
//! boundary — statements reach expressions through `expression` and `lambda`,
//! and expressions reach back only for `as_pattern`, `sequence` and `block` —
//! so the split follows the grammar's own division rather than cutting through
//! the normalizer's state, which both halves still share through `Normalizer`.

use infact_normalize::{Direction, Form, Pattern};
use tree_sitter::Node;

use super::{CONTAINERS, Normalizer, code_children, is_screaming_case, pattern_is, valued};

impl Normalizer<'_> {
    pub(crate) fn expression(&mut self, node: Node<'_>) -> Form {
        match node.kind() {
            "parenthesized_expression" => match code_children(node).first().copied() {
                Some(inner) => self.expression(inner),
                None => Form::Literal,
            },
            "identifier" => {
                let name = self.text(node);
                // A name in screaming case that nothing here binds is a named
                // constant, and which constant it is *is* behavior. The
                // TypeScript frontend paid for this once: resolving both to a
                // hole made `ITEM_KIND_KEY` and `ITEM_KIND_VALUE` one thing, and
                // with them `keys` and `values`. Python spells constants the
                // same way and inherits the same erasure.
                if is_screaming_case(name) && !self.roles.is_value(name) {
                    return self.path(name);
                }
                self.roles.resolve(name)
            }
            "integer" | "float" => Form::Number(self.text(node).to_owned()),
            "true" | "false" => Form::Constant(self.text(node).to_owned()),
            // `None` is the absence a caller has to handle, and that is exactly
            // what `Option::None` and `undefined` are elsewhere.
            "none" => Form::Variant {
                name: "None".to_owned(),
                payload: Vec::new(),
            },
            "ellipsis" => Form::Literal,
            "string" | "concatenated_string" => Form::Constant(self.text(node).to_owned()),
            // An EMPTY literal is construction and its elements are what it
            // does not have. A literal with elements carries them, and holding
            // it as a bare `Construct` would make `(a, b)` and `(b, a)` one
            // value — which is exactly the erasure that made `keys` and
            // `values` the same behavior in the Rust frontend.
            "list" => self.literal(node, "list"),
            "tuple" => self.literal(node, "tuple"),
            "set" => self.literal(node, "set"),
            "dictionary" => self.literal(node, "dict"),
            "pair" => Form::Variant {
                name: "pair".to_owned(),
                payload: vec![
                    node.child_by_field_name("key")
                        .map_or(Form::Literal, |key| self.expression(key)),
                    node.child_by_field_name("value")
                        .map_or(Form::Literal, |value| self.expression(value)),
                ],
            },
            "list_comprehension" => self.comprehension(node, Some("list")),
            "set_comprehension" => self.comprehension(node, Some("set")),
            "dictionary_comprehension" => self.comprehension(node, Some("dict")),
            // A generator produces the sequence without gathering it, which is
            // the whole difference between it and a list comprehension.
            "generator_expression" => self.comprehension(node, None),
            "unary_operator" | "not_operator" => self.unary(node),
            "binary_operator" | "boolean_operator" => self.binary(node),
            "comparison_operator" => self.comparison(node),
            "conditional_expression" => self.conditional(node),
            "call" => self.call(node),
            "attribute" => self.attribute(node),
            "subscript" => self.subscript(node),
            "lambda" => self.lambda(node),
            "assignment" | "augmented_assignment" => self.assignment(node),
            "named_expression" => self.walrus(node),
            "await" | "list_splat" | "dictionary_splat" | "type_conversion" => {
                match code_children(node).first().copied() {
                    Some(inner) => self.expression(inner),
                    None => Form::Literal,
                }
            }
            "yield" => Form::Variant {
                name: "Yield".to_owned(),
                payload: code_children(node)
                    .into_iter()
                    .map(|child| self.expression(child))
                    .collect(),
            },
            "keyword_argument" => match node.child_by_field_name("value") {
                Some(value) => self.expression(value),
                None => Form::Literal,
            },
            // `X as y` reached as an expression rather than through a `with` or
            // an `except`: still a name given to a value.
            "as_pattern" => match self.as_pattern(node) {
                Some((subject, alias)) => {
                    let value = self.expression(subject);
                    let pattern = self.bind_pattern(alias);
                    Form::Let {
                        pattern: Box::new(pattern),
                        value: Box::new(value),
                    }
                }
                None => Form::Literal,
            },
            "expression_list" => Form::Sequence(
                code_children(node)
                    .into_iter()
                    .map(|child| self.expression(child))
                    .collect(),
            ),
            _ => Form::Opaque {
                kind: node.kind().to_owned(),
                parts: code_children(node)
                    .into_iter()
                    .map(|child| self.expression(child))
                    .collect(),
            },
        }
    }

    fn unary(&mut self, node: Node<'_>) -> Form {
        let Some(operand) = node.child_by_field_name("argument") else {
            return Form::Literal;
        };
        let operand = self.expression(operand);
        let operator = if node.kind() == "not_operator" {
            "not"
        } else {
            node.child_by_field_name("operator")
                .map_or("", |operator| self.text(operator))
        };
        match operator {
            // Unary plus is the value it was given.
            "" | "+" => operand,
            _ => Form::Binary {
                operator: operator.to_owned(),
                left: Box::new(operand),
                right: Box::new(Form::Literal),
            },
        }
    }

    fn binary(&mut self, node: Node<'_>) -> Form {
        let operator = node
            .child_by_field_name("operator")
            .map_or(String::new(), |operator| self.text(operator).to_owned());
        let left = node
            .child_by_field_name("left")
            .map_or(Form::Literal, |left| self.expression(left));
        let right = node
            .child_by_field_name("right")
            .map_or(Form::Literal, |right| self.expression(right));
        Form::Binary {
            operator,
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    /// `a < b < c` is `a < b and b < c`, and Python is the only language here
    /// that spells it in one node.
    ///
    /// Expanding it is what makes the chained form and the written-out form one
    /// thing. The operands are not evaluated twice at run time, and this form
    /// says nothing about how many times anything ran.
    fn comparison(&mut self, node: Node<'_>) -> Form {
        let mut cursor = node.walk();
        // Anonymous children are the operators and are what this reads
        // positionally, so a `\` continuation left among them would shift every
        // index by one and hand an operator token to `expression`. Measured: it
        // did, on 31 comparisons across CPython's standard library.
        let parts = node
            .children(&mut cursor)
            .filter(|child| !matches!(child.kind(), "comment" | "line_continuation"))
            .collect::<Vec<_>>();
        let mut comparisons: Vec<Form> = Vec::new();
        let mut index = 0;
        while index + 2 < parts.len() {
            let (left, right) = (parts[index], parts[index + 2]);
            // `not in` and `is not` are two tokens each, so the operator is
            // whatever text sits between the operands rather than one child.
            let operator = self
                .source
                .get(left.end_byte()..right.start_byte())
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if operator.is_empty() {
                break;
            }
            let left = self.expression(left);
            let right = self.expression(right);
            comparisons.push(Form::Binary {
                operator,
                left: Box::new(left),
                right: Box::new(right),
            });
            index += 2;
        }
        let mut comparisons = comparisons.into_iter();
        let Some(first) = comparisons.next() else {
            return Form::Literal;
        };
        comparisons.fold(first, |left, right| Form::Binary {
            operator: "and".to_owned(),
            left: Box::new(left),
            right: Box::new(right),
        })
    }

    fn conditional(&mut self, node: Node<'_>) -> Form {
        // `x if c else y` writes the consequence first, and the grammar gives
        // the three parts no field names at all.
        let parts = code_children(node);
        let [consequence, condition, alternative] = parts.as_slice() else {
            return Form::Literal;
        };
        let condition = self.expression(*condition);
        let consequence = self.expression(*consequence);
        let alternative = self.expression(*alternative);
        Form::Branch {
            condition: Box::new(condition),
            consequence: Box::new(consequence),
            alternative: Some(Box::new(alternative)),
        }
    }

    /// A comprehension, in every spelling Python has for one.
    ///
    /// `[g(x) for x in xs if p(x)]` is a walk that keeps some elements and
    /// transforms them, which is exactly what the three-statement loop the
    /// cleanup pass rewrites also says. An `if` clause becomes a `Retain`
    /// around the sequence rather than a `Branch` in the body, because an
    /// element that fails the test is not produced at all.
    fn comprehension(&mut self, node: Node<'_>, container: Option<&str>) -> Form {
        let clauses = code_children(node);
        let Some(body) = clauses.first().copied() else {
            return Form::Literal;
        };
        // Every clause is bound before the body is read, because a later clause
        // can name what an earlier one bound and the body names all of them.
        let mut sequence = None;
        let mut item = Pattern::Ignored;
        let mut direction = Direction::Forward;
        let mut conditions = Vec::new();
        for clause in &clauses[1..] {
            match clause.kind() {
                "for_in_clause" => {
                    let Some(right) = clause.child_by_field_name("right") else {
                        continue;
                    };
                    let (walked, walked_direction) = self.sequence(right);
                    let bound = clause
                        .child_by_field_name("left")
                        .map_or(Pattern::Ignored, |left| self.bind_pattern(left));
                    match sequence.take() {
                        // A second `for` walks something each element of the
                        // first produced, which is a flattening.
                        Some(outer) => {
                            sequence = Some(Form::Sift {
                                sequence: Box::new(outer),
                                item: Box::new(std::mem::replace(&mut item, bound)),
                                body: Box::new(walked),
                            });
                        }
                        None => {
                            sequence = Some(walked);
                            item = bound;
                            direction = walked_direction;
                        }
                    }
                }
                "if_clause" => {
                    if let Some(condition) = code_children(*clause).first().copied() {
                        conditions.push(self.expression(condition));
                    }
                }
                _ => {}
            }
        }
        let Some(mut walked) = sequence else {
            return Form::Literal;
        };
        if direction == Direction::Backward {
            // `Traverse` carries a direction and a bare sequence does not, so
            // the reversal is held where it cannot be mistaken for a hole.
            walked = Form::Opaque {
                kind: "reversed".to_owned(),
                parts: vec![walked],
            };
        }
        let mut conditions = conditions.into_iter();
        if let Some(first) = conditions.next() {
            let condition = conditions.fold(first, |left, right| Form::Binary {
                operator: "and".to_owned(),
                left: Box::new(left),
                right: Box::new(right),
            });
            walked = Form::Retain {
                sequence: Box::new(walked),
                item: Box::new(item.clone()),
                body: Box::new(condition),
            };
        }
        let produced = self.expression(body);
        // `[x for x in xs]` transforms nothing, and saying it does would make a
        // copy look like work.
        let sequence = if pattern_is(&item, &produced) {
            walked
        } else {
            Form::Transform {
                sequence: Box::new(walked),
                item: Box::new(item),
                body: Box::new(produced),
            }
        };
        match container {
            Some(container) => Form::Collect {
                sequence: Box::new(sequence),
                container: Some(container.to_owned()),
            },
            None => sequence,
        }
    }

    fn call(&mut self, node: Node<'_>) -> Form {
        let Some(callee) = node.child_by_field_name("function") else {
            return Form::Literal;
        };
        let arguments = node
            .child_by_field_name("arguments")
            .map(|arguments| match arguments.kind() {
                // `sum(x for x in xs)` writes the generator where the argument
                // list would be.
                "generator_expression" => vec![arguments],
                _ => code_children(arguments),
            })
            .unwrap_or_default();

        // A method call keeps its receiver, because which value a method was
        // called on is most of what the call says.
        if callee.kind() == "attribute"
            && let Some(object) = callee.child_by_field_name("object")
            && let Some(name) = callee.child_by_field_name("attribute")
        {
            let receiver = self.expression(object);
            return Form::Method {
                name: self.text(name).to_owned(),
                receiver: Box::new(receiver),
                arguments: arguments
                    .iter()
                    .map(|argument| self.expression(*argument))
                    .collect(),
            };
        }

        // `list(xs)`, `set(xs)`, `dict(pairs)` gather a sequence into a named
        // container, which is what `.collect()` and `Array.from` are elsewhere.
        if callee.kind() == "identifier"
            && CONTAINERS.contains(&self.text(callee))
            && !self.roles.is_value(self.text(callee))
            && let [only] = arguments.as_slice()
        {
            let container = self.text(callee).to_owned();
            let sequence = self.expression(*only);
            return Form::Collect {
                sequence: Box::new(sequence),
                container: Some(container),
            };
        }

        // Calling a function defined elsewhere is not the same as calling one
        // the caller supplied. `helper(x)` names something a reader can go and
        // look at, and two delegations to different helpers are two behaviors.
        //
        // `infact-ts-normalize` does this at the same point, and Python needs it
        // more than TypeScript does: a class is constructed by naming it, so
        // `_UnixReadPipeTransport(self, pipe, protocol, waiter, extra)` and
        // `_ProactorReadPipeTransport(...)` differ in nothing but the name, and
        // reduced to the same six-hole form until this existed. Measured over
        // the installed corpus, 94.9% of calls had a hole for a callee.
        //
        // The name is kept bare rather than qualified by module. Two packages
        // that both define `Regex` do collide under this, which is the same
        // trade the TypeScript frontend made; qualifying would need to resolve
        // imports across files, and that is a different observation.
        let callee = match callee.kind() {
            "identifier" if !self.roles.is_value(self.text(callee)) => self.path(self.text(callee)),
            _ => self.expression(callee),
        };
        Form::Call {
            callee: Box::new(callee),
            arguments: arguments
                .iter()
                .map(|argument| self.expression(*argument))
                .collect(),
        }
    }

    /// A collection literal: construction when it is empty, the values it holds
    /// when it is not.
    ///
    /// `fuse_container_fills` recognizes the first shape, so keeping empty
    /// literals as bare `Construct` is what lets an append loop be seen at all.
    fn literal(&mut self, node: Node<'_>, container: &str) -> Form {
        let elements = code_children(node);
        if elements.is_empty() {
            return Form::Construct(container.to_owned());
        }
        Form::Variant {
            name: container.to_owned(),
            payload: elements
                .into_iter()
                .map(|element| self.expression(element))
                .collect(),
        }
    }

    fn attribute(&mut self, node: Node<'_>) -> Form {
        let Some(object) = node.child_by_field_name("object") else {
            return Form::Literal;
        };
        let Some(name) = node.child_by_field_name("attribute") else {
            return Form::Literal;
        };
        let value = self.expression(object);
        Form::Field {
            value: Box::new(value),
            name: self.text(name).to_owned(),
        }
    }

    /// `xs[i]` takes an element and `xs[a:b]` takes a run of them.
    ///
    /// Holding the second as one opaque node accounted for 72% of everything
    /// this crate could not read across CPython's standard library and the
    /// installed site-packages — 11,243 of 15,620 opaque nodes — which is what
    /// says it was worth a rule rather than a note.
    fn subscript(&mut self, node: Node<'_>) -> Form {
        let Some(value) = node.child_by_field_name("value") else {
            return Form::Literal;
        };
        let receiver = self.expression(value);
        let Some(index) = node.child_by_field_name("subscript") else {
            return Form::Method {
                name: "index".to_owned(),
                receiver: Box::new(receiver),
                arguments: vec![Form::Literal],
            };
        };
        if index.kind() != "slice" {
            let index = self.expression(index);
            return Form::Method {
                name: "index".to_owned(),
                receiver: Box::new(receiver),
                arguments: vec![index],
            };
        }
        // A bound the source left out is an end of the sequence, and it has to
        // be held rather than dropped: `xs[1:]` and `xs[:1]` are opposite
        // halves, and a form that kept only the `1` would make them one thing.
        let mut cursor = index.walk();
        let mut bounds = Vec::new();
        let mut pending = true;
        for part in index.children(&mut cursor) {
            if part.kind() == ":" {
                if pending {
                    bounds.push(Form::Variant {
                        name: "None".to_owned(),
                        payload: Vec::new(),
                    });
                }
                pending = true;
                continue;
            }
            if matches!(part.kind(), "comment" | "line_continuation") {
                continue;
            }
            bounds.push(self.expression(part));
            pending = false;
        }
        if pending {
            bounds.push(Form::Variant {
                name: "None".to_owned(),
                payload: Vec::new(),
            });
        }
        Form::Method {
            name: "slice".to_owned(),
            receiver: Box::new(receiver),
            arguments: bounds,
        }
    }

    pub(crate) fn lambda(&mut self, node: Node<'_>) -> Form {
        let parameters = node
            .child_by_field_name("parameters")
            .map(|parameters| self.bind_parameters(parameters))
            .unwrap_or_default();
        let body = match node.child_by_field_name("body") {
            Some(body) if body.kind() == "block" => valued(self.block(body)),
            Some(body) => self.expression(body),
            None => Form::Sequence(Vec::new()),
        };
        Form::Lambda {
            parameters,
            body: Box::new(body),
        }
    }

    /// Bind a parameter list, in either spelling, and report the patterns.
    fn bind_parameters(&mut self, parameters: Node<'_>) -> Vec<Pattern> {
        code_children(parameters)
            .into_iter()
            .filter_map(|parameter| match parameter.kind() {
                // `/` and `*` say how a caller may pass arguments, not what
                // the function does with them.
                "positional_separator" | "keyword_separator" => None,
                "identifier" => Some(self.bind_pattern(parameter)),
                "default_parameter" | "typed_default_parameter" | "typed_parameter" => parameter
                    .child_by_field_name("name")
                    .or_else(|| code_children(parameter).first().copied())
                    .map(|name| self.bind_pattern(name)),
                "list_splat_pattern" | "dictionary_splat_pattern" | "tuple_pattern" => {
                    Some(self.bind_pattern(parameter))
                }
                _ => None,
            })
            .collect()
    }

    fn assignment(&mut self, node: Node<'_>) -> Form {
        let Some(target) = node.child_by_field_name("left") else {
            return Form::Literal;
        };
        let Some(value) = node.child_by_field_name("right") else {
            // `x: int` with no value is an annotation and binds nothing.
            return Form::Literal;
        };
        let value = self.expression(value);
        if node.kind() == "augmented_assignment" {
            let operator = node
                .child_by_field_name("operator")
                .map_or(String::new(), |operator| self.text(operator).to_owned());
            let target = self.expression(target);
            return Form::Assign {
                operator,
                target: Box::new(target),
                value: Box::new(value),
            };
        }
        // Storing into an attribute or a subscript writes to something that
        // already exists; binding a name introduces one, and downstream the
        // two are different things.
        if !matches!(
            target.kind(),
            "identifier" | "pattern_list" | "tuple_pattern" | "list_pattern"
        ) {
            let target = self.expression(target);
            return Form::Assign {
                operator: "=".to_owned(),
                target: Box::new(target),
                value: Box::new(value),
            };
        }
        let pattern = self.bind_pattern(target);
        Form::Let {
            pattern: Box::new(pattern),
            value: Box::new(value),
        }
    }

    /// `(n := f())` binds and yields the value in one breath.
    fn walrus(&mut self, node: Node<'_>) -> Form {
        let value = node
            .child_by_field_name("value")
            .map_or(Form::Literal, |value| self.expression(value));
        let Some(name) = node.child_by_field_name("name") else {
            return value;
        };
        let pattern = self.bind_pattern(name);
        Form::Let {
            pattern: Box::new(pattern),
            value: Box::new(value),
        }
    }
}
