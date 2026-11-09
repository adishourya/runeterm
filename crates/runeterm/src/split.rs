//! The split tree of one tab, as spooky's layout stores it: a leaf is a pane, a split puts `a`
//! left of (or above) `b`, and `ratio` is `a`'s share.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// Side by side (spooky's "h").
    Right,
    /// Stacked (spooky's "v").
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Leaf(u32),
    Split { dir: Dir, ratio: f32, a: Box<Node>, b: Box<Node> },
}

/// Where a removed leaf was: beside which leaf, in which direction, with what share.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    pub beside: u32,
    pub dir: Dir,
    pub share: f32,
    pub first: bool,
}

type Rect = [f32; 4]; // x, y, w, h in the unit square

impl Node {
    pub fn contains(&self, id: u32) -> bool {
        match self {
            Node::Leaf(l) => *l == id,
            Node::Split { a, b, .. } => a.contains(id) || b.contains(id),
        }
    }

    pub fn first(&self) -> u32 {
        match self {
            Node::Leaf(l) => *l,
            Node::Split { a, .. } => a.first(),
        }
    }

    pub fn leaves(&self) -> Vec<u32> {
        let mut out = vec![];
        self.rects([0.0, 0.0, 1.0, 1.0], &mut |id, _| out.push(id));
        out
    }

    /// Replaces leaf `at` with a split holding it and `new` (after it, in `dir`).
    pub fn split(&mut self, at: u32, new: u32, dir: Dir) {
        match self {
            Node::Leaf(l) if *l == at => {
                *self = Node::Split { dir, ratio: 0.5, a: Box::new(Node::Leaf(at)), b: Box::new(Node::Leaf(new)) };
            }
            Node::Leaf(_) => {}
            Node::Split { a, b, .. } => {
                a.split(at, new, dir);
                b.split(at, new, dir);
            }
        }
    }

    /// Sets the ratio of the split at `path` (false: into `a`, true: into `b`).
    pub fn set_ratio(&mut self, path: &[bool], r: f32) {
        if let Node::Split { ratio, a, b, .. } = self {
            match path.split_first() {
                None => *ratio = r,
                Some((false, rest)) => a.set_ratio(rest, r),
                Some((true, rest)) => b.set_ratio(rest, r),
            }
        }
    }

    /// Swaps two leaves' places.
    pub fn swap(&mut self, a: u32, b: u32) {
        match self {
            Node::Leaf(l) if *l == a => *l = b,
            Node::Leaf(l) if *l == b => *l = a,
            Node::Leaf(_) => {}
            Node::Split { a: x, b: y, .. } => {
                x.swap(a, b);
                y.swap(a, b);
            }
        }
    }

    /// Where leaf `id` sits, so it can be put back after `remove`: next to a leaf of its
    /// sibling subtree, on the same side, at the same share.
    pub fn place_of(&self, id: u32) -> Option<Place> {
        let Node::Split { dir, ratio, a, b } = self else { return None };
        match (&**a, &**b) {
            (Node::Leaf(l), other) if *l == id => Some(Place { beside: other.first(), dir: *dir, share: *ratio, first: true }),
            (other, Node::Leaf(l)) if *l == id => Some(Place { beside: other.first(), dir: *dir, share: 1.0 - ratio, first: false }),
            _ => a.place_of(id).or_else(|| b.place_of(id)),
        }
    }

    /// Splits leaf `at` to hold `new` with `share` of the space, before or after it.
    pub fn insert(&mut self, at: u32, new: u32, dir: Dir, share: f32, first: bool) -> bool {
        match self {
            Node::Leaf(l) if *l == at => {
                let (a, b, ratio) = if first { (new, at, share) } else { (at, new, 1.0 - share) };
                *self = Node::Split { dir, ratio, a: Box::new(Node::Leaf(a)), b: Box::new(Node::Leaf(b)) };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => a.insert(at, new, dir, share, first) || b.insert(at, new, dir, share, first),
        }
    }

    /// The tree without leaf `id`; None when it was the only leaf.
    pub fn remove(&self, id: u32) -> Option<Node> {
        match self {
            Node::Leaf(l) => (*l != id).then(|| self.clone()),
            Node::Split { dir, ratio, a, b } => match (a.remove(id), b.remove(id)) {
                (Some(a), Some(b)) => Some(Node::Split { dir: *dir, ratio: *ratio, a: Box::new(a), b: Box::new(b) }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            },
        }
    }

    fn rects(&self, r: Rect, f: &mut dyn FnMut(u32, Rect)) {
        match self {
            Node::Leaf(id) => f(*id, r),
            Node::Split { dir: Dir::Right, ratio, a, b } => {
                let w = r[2] * ratio;
                a.rects([r[0], r[1], w, r[3]], f);
                b.rects([r[0] + w, r[1], r[2] - w, r[3]], f);
            }
            Node::Split { dir: Dir::Down, ratio, a, b } => {
                let h = r[3] * ratio;
                a.rects([r[0], r[1], r[2], h], f);
                b.rects([r[0], r[1] + h, r[2], r[3] - h], f);
            }
        }
    }

    /// The pane next to `id` on `side`: it must touch that edge and overlap `id` across it;
    /// the one with the most overlap wins.
    pub fn neighbor(&self, id: u32, side: Side) -> Option<u32> {
        let mut all = vec![];
        self.rects([0.0, 0.0, 1.0, 1.0], &mut |i, r| all.push((i, r)));
        let (_, me) = *all.iter().find(|(i, _)| *i == id)?;
        let eps = 1e-4;
        let overlap = |a0: f32, a1: f32, b0: f32, b1: f32| (a1.min(b1) - a0.max(b0)).max(0.0);
        all.iter()
            .filter(|(i, _)| *i != id)
            .filter_map(|&(i, r)| {
                let touches = match side {
                    Side::Left => (r[0] + r[2] - me[0]).abs() < eps,
                    Side::Right => (me[0] + me[2] - r[0]).abs() < eps,
                    Side::Up => (r[1] + r[3] - me[1]).abs() < eps,
                    Side::Down => (me[1] + me[3] - r[1]).abs() < eps,
                };
                let o = match side {
                    Side::Left | Side::Right => overlap(me[1], me[1] + me[3], r[1], r[1] + r[3]),
                    Side::Up | Side::Down => overlap(me[0], me[0] + me[2], r[0], r[0] + r[2]),
                };
                (touches && o > eps).then_some((i, o))
            })
            // Most overlap wins; on a tie, the first (top or left) one.
            .fold(None, |best: Option<(u32, f32)>, c| match best {
                Some(b) if b.1 >= c.1 - 1e-6 => Some(b),
                _ => Some(c),
            })
            .map(|(i, _)| i)
    }

    /// Moves the divider nearest `id` along `side`'s axis by `by` (a share of that split).
    /// Returns whether a split was found.
    pub fn resize(&mut self, id: u32, side: Side, by: f32) -> bool {
        let Node::Split { dir, ratio, a, b } = self else { return false };
        let inner = if a.contains(id) { a.resize(id, side, by) } else if b.contains(id) { b.resize(id, side, by) } else { return false };
        if inner {
            return true;
        }
        let sign = match (side, *dir) {
            (Side::Right, Dir::Right) | (Side::Down, Dir::Down) => 1.0,
            (Side::Left, Dir::Right) | (Side::Up, Dir::Down) => -1.0,
            _ => return false,
        };
        *ratio = (*ratio + sign * by).clamp(0.1, 0.9);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn three() -> Node {
        // 1 | 2
        //   | 3
        let mut n = Node::Leaf(1);
        n.split(1, 2, Dir::Right);
        n.split(2, 3, Dir::Down);
        n
    }

    #[test]
    fn splits_and_removes() {
        let n = three();
        assert_eq!(n.leaves(), vec![1, 2, 3]);
        let n = n.remove(2).unwrap();
        assert_eq!(n.leaves(), vec![1, 3]);
        assert_eq!(Node::Leaf(1).remove(1), None);
    }

    #[test]
    fn swaps_and_puts_back() {
        let mut n = three();
        n.swap(1, 3);
        assert_eq!(n.leaves(), vec![3, 2, 1]);
        let n = three();
        let place = n.place_of(3).unwrap();
        assert_eq!(place, Place { beside: 2, dir: Dir::Down, share: 0.5, first: false });
        let mut rest = n.remove(3).unwrap();
        assert!(rest.insert(place.beside, 3, place.dir, place.share, place.first));
        assert_eq!(rest, three());
    }

    #[test]
    fn finds_neighbors_by_geometry() {
        let n = three();
        assert_eq!(n.neighbor(1, Side::Right), Some(2));
        assert_eq!(n.neighbor(3, Side::Left), Some(1));
        assert_eq!(n.neighbor(2, Side::Down), Some(3));
        assert_eq!(n.neighbor(3, Side::Up), Some(2));
        assert_eq!(n.neighbor(1, Side::Left), None);
    }

    #[test]
    fn resizes_the_nearest_divider_on_that_axis() {
        let mut n = three();
        assert!(n.resize(3, Side::Up, 0.1)); // the 2/3 divider moves up
        let Node::Split { b, ratio, .. } = &n else { panic!() };
        assert_eq!(*ratio, 0.5);
        let Node::Split { ratio, .. } = &**b else { panic!() };
        assert!((ratio - 0.4).abs() < 1e-6);
        assert!(n.resize(3, Side::Left, 0.1)); // no horizontal split below: the root's moves
        let Node::Split { ratio, .. } = &n else { panic!() };
        assert!((ratio - 0.4).abs() < 1e-6);
    }
}
