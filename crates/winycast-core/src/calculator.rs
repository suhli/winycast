pub fn calculate(query: &str) -> Option<String> {
    let query = query.trim();
    let (source, explicit) = query
        .strip_prefix('=')
        .map_or((query, false), |text| (text.trim(), true));
    if source.len() > 512
        || (!explicit && !source.contains(['+', '-', '*', '/', '%', '^', '(', '×', '÷']))
    {
        return None;
    }
    let normalized = source.replace('×', "*").replace('÷', "/").replace('−', "-");
    let mut parser = Parser(normalized.as_bytes());
    let value = parser.expression(0, 0)?;
    (parser.peek().is_none() && value.is_finite()).then(|| {
        if value == 0. {
            return "0".into();
        }
        if !(1e-8..1e15).contains(&value.abs()) {
            return format!("{value:.10e}");
        }
        format!("{value:.10}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned()
    })
}

struct Parser<'a>(&'a [u8]);

impl Parser<'_> {
    fn peek(&mut self) -> Option<u8> {
        self.0 = self.0.trim_ascii_start();
        self.0.first().copied()
    }

    fn take(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.0 = &self.0[1..];
            true
        } else {
            false
        }
    }

    fn expression(&mut self, precedence: u8, depth: u8) -> Option<f64> {
        if depth >= 40 {
            return None;
        }
        let mut left = match self.peek()? {
            b'+' => {
                self.take(b'+');
                self.expression(5, depth + 1)?
            }
            b'-' => {
                self.take(b'-');
                -self.expression(5, depth + 1)?
            }
            b'(' => {
                self.take(b'(');
                let value = self.expression(0, depth + 1)?;
                self.take(b')').then_some(value)?
            }
            c if c.is_ascii_alphabetic() => self.function(depth)?,
            _ => self.number()?,
        };
        while let Some(operator) = self.peek() {
            let (lower, upper) = match operator {
                b'+' | b'-' => (1, 2),
                b'*' | b'/' | b'%' => (3, 4),
                b'^' => (6, 5),
                _ => break,
            };
            if lower < precedence {
                break;
            }
            self.take(operator);
            let right = self.expression(upper, depth + 1)?;
            left = match operator {
                b'+' => left + right,
                b'-' => left - right,
                b'*' => left * right,
                b'/' => left / right,
                b'%' => left % right,
                _ => left.powf(right),
            };
        }
        Some(left)
    }

    fn number(&mut self) -> Option<f64> {
        let mut n = self
            .0
            .iter()
            .take_while(|c| c.is_ascii_digit() || **c == b'.')
            .count();
        if n > 0 && matches!(self.0.get(n), Some(b'e' | b'E')) {
            n += 1;
            if matches!(self.0.get(n), Some(b'+' | b'-')) {
                n += 1;
            }
            n += self.0[n..]
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .count();
        }
        let (number, remainder) = self.0.split_at(n);
        self.0 = remainder;
        std::str::from_utf8(number).ok()?.parse().ok()
    }

    fn function(&mut self, depth: u8) -> Option<f64> {
        let n = self
            .0
            .iter()
            .take_while(|c| c.is_ascii_alphabetic())
            .count();
        let (name, remainder) = self.0.split_at(n);
        self.0 = remainder;
        match name {
            b"pi" => return Some(std::f64::consts::PI),
            b"e" => return Some(std::f64::consts::E),
            _ => {}
        }
        if !self.take(b'(') {
            return None;
        }
        let arg = self.expression(0, depth + 1)?;
        if !self.take(b')') {
            return None;
        }
        Some(match name {
            b"sqrt" => arg.sqrt(),
            b"abs" => arg.abs(),
            b"sin" => arg.sin(),
            b"cos" => arg.cos(),
            b"tan" => arg.tan(),
            b"ln" => arg.ln(),
            b"log" => arg.log10(),
            b"round" => arg.round(),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expressions_respect_precedence_and_reject_invalid_values() {
        for (text, result) in [
            ("2+3×4", "14"),
            ("2^3^2", "512"),
            ("-2^2", "-4"),
            ("2^-2", "0.25"),
            ("= sqrt(16) + 2e-1", "4.2"),
            ("=sin(pi/2)", "1"),
        ] {
            assert_eq!(calculate(text).as_deref(), Some(result));
        }
        for text in [
            "notepad",
            "42",
            "2 / 0",
            "=sqrt(-1)",
            "2+3 garbage",
            "=2..3",
        ] {
            assert_eq!(calculate(text), None);
        }
    }
}
