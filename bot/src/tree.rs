use rand::prelude::*;
use enum_map::EnumMap;
use arrayvec::ArrayVec;

use libtetris::{ Board, LockResult, Piece, FallingPiece };
use crate::moves::Move;

pub struct Tree {
    pub board: Board,
    pub raw_eval: crate::evaluation::Evaluation,
    pub evaluation: i32,
    pub depth: usize,
    pub child_nodes: usize,
    kind: Option<TreeKind>
}

enum TreeKind {
    Known(Vec<Child>),
    Unknown(Speculation)
}

type Speculation = EnumMap<Piece, Option<Vec<Child>>>;

pub struct Child {
    pub hold: bool,
    pub mv: Move,
    pub lock: LockResult,
    pub tree: Tree
}

impl Tree {
    pub fn new(
        board: Board,
        lock: &LockResult,
        soft_dropped: bool,
        transient_weights: &crate::evaluation::BoardWeights,
        acc_weights: &crate::evaluation::PlacementWeights
    ) -> Self {
        use crate::evaluation::evaluate;
        let raw_eval = evaluate(lock, &board, transient_weights, acc_weights, soft_dropped);
        Tree {
            raw_eval, board,
            evaluation: raw_eval.accumulated + raw_eval.transient,
            depth: 0,
            child_nodes: 0,
            kind: None
        }
    }

    pub fn into_best_child(mut self) -> Result<Child, Tree> {
        match self.kind {
            None => Err(self),
            Some(tk) => {
                match tk.into_best_child() {
                    Ok(c) => Ok(c),
                    Err(tk) => {
                        self.kind = Some(tk);
                        Err(self)
                    }
                }
            }
        }
    }

    pub fn add_next_piece(&mut self, piece: Piece) {
        self.board.add_next_piece(piece);
        if let Some(ref mut k) = self.kind {
            k.add_next_piece(piece);
        }
    }

    pub fn extend(
        &mut self,
        mode: crate::moves::MovementMode,
        transient_weights: &crate::evaluation::BoardWeights,
        acc_weights: &crate::evaluation::PlacementWeights
    ) -> bool {
        self.expand(mode, transient_weights, acc_weights).is_death
    }

    fn expand(
        &mut self,
        mode: crate::moves::MovementMode,
        transient_weights: &crate::evaluation::BoardWeights,
        acc_weights: &crate::evaluation::PlacementWeights
    ) -> ExpandResult {
        match self.kind {
            Some(ref mut tk) => {
                let er = tk.expand(mode, transient_weights, acc_weights);
                if !er.is_death {
                    self.evaluation = tk.evaluation() + self.raw_eval.accumulated;
                    self.depth = self.depth.max(er.depth);
                    self.child_nodes += er.new_nodes;
                }
                er
            }
            None => {
                if self.board.get_next_piece().is_ok() {
                    if self.board.hold_piece().is_none() &&
                            self.board.get_next_next_piece().is_none() {
                        // Speculate - next piece is known, but hold piece isn't
                        self.speculate(
                            mode, transient_weights, acc_weights
                        )
                    } else {
                        // Both next piece and hold piece are known
                        let children = new_children(
                            self.board.clone(), mode, transient_weights, acc_weights
                        );

                        if children.is_empty() {
                            ExpandResult {
                                is_death: true,
                                depth: 0,
                                new_nodes: 0
                            }
                        } else {
                            self.depth = 1;
                            self.child_nodes = children.len();
                            let tk = TreeKind::Known(children);
                            self.evaluation = tk.evaluation() + self.raw_eval.accumulated;
                            self.kind = Some(tk);
                            ExpandResult {
                                is_death: false,
                                depth: 1,
                                new_nodes: self.child_nodes
                            }
                        }
                    }
                } else {
                    // Speculate - hold should be known, but next piece isn't
                    if self.board.hold_piece().is_some() {
                        self.speculate(mode, transient_weights, acc_weights)
                    } else {
                        ExpandResult {
                            is_death: false, depth: 0, new_nodes: 0
                        }
                    }
                }
            }
        }
    }

    fn speculate(
        &mut self,
        mode: crate::moves::MovementMode,
        transient_weights: &crate::evaluation::BoardWeights,
        acc_weights: &crate::evaluation::PlacementWeights
    ) -> ExpandResult {
        let possibilities = match self.board.get_next_piece() {
            Ok(_) => {
                let mut b = self.board.clone();
                b.advance_queue();
                b.get_next_piece().unwrap_err()
            }
            Err(possibilities) => possibilities
        };
        let mut speculation = EnumMap::new();
        for piece in possibilities.iter() {
            let mut board = self.board.clone();
            board.add_next_piece(piece);
            let children = new_children(
                board, mode, transient_weights, acc_weights
            );
            self.child_nodes += children.len();
            speculation[piece] = Some(children);
        }

        if self.child_nodes == 0 {
            ExpandResult {
                is_death: true,
                depth: 0,
                new_nodes: 0
            }
        } else {
            let tk = TreeKind::Unknown(speculation);
            self.evaluation = tk.evaluation() + self.raw_eval.accumulated;
            self.kind = Some(tk);
            self.depth = 1;
            ExpandResult {
                is_death: false,
                depth: 1,
                new_nodes: self.child_nodes
            }
        }
    }
}

/// Expect: If there is no hold piece, there are at least 2 pieces in the queue.
/// Otherwise there is at least 1 piece in the queue.
fn new_children(
    mut board: Board,
    mode: crate::moves::MovementMode,
    transient_weights: &crate::evaluation::BoardWeights,
    acc_weights: &crate::evaluation::PlacementWeights
) -> Vec<Child> {
    let mut children = vec![];
    let next = board.advance_queue().unwrap();
    let spawned = match FallingPiece::spawn(next, &board) {
        Some(s) => s,
        None => return children
    };

    // Placements for next piece
    for mv in crate::moves::find_moves(&board, spawned, mode) {
        let mut board = board.clone();
        let lock = board.lock_piece(mv.location);
        if !lock.locked_out {
            children.push(Child {
                tree: Tree::new(board, &lock, mv.soft_dropped, transient_weights, acc_weights),
                hold: false,
                mv, lock
            })
        }
    }

    let mut board = board.clone();
    let hold = board.hold(next).unwrap_or_else(|| board.advance_queue().unwrap());
    if let Some(spawned) = FallingPiece::spawn(hold, &board) {
        // Placements for hold piece
        for mv in crate::moves::find_moves(&board, spawned, mode) {
            let mut board = board.clone();
            let lock = board.lock_piece(mv.location);
            if !lock.locked_out {
                children.push(Child {
                    tree: Tree::new(board, &lock, mv.soft_dropped, transient_weights, acc_weights),
                    hold: true,
                    mv, lock
                })
            }
        }
    }

    children.sort_by_key(|child| -child.tree.evaluation);
    children
}

struct ExpandResult {
    depth: usize,
    new_nodes: usize,
    is_death: bool
}

impl TreeKind {
    fn into_best_child(self) -> Result<Child, TreeKind> {
        match self {
            TreeKind::Known(children) => if children.is_empty() {
                Err(TreeKind::Known(children))
            } else {
                Ok(children.into_iter().next().unwrap())
            },
            TreeKind::Unknown(_) => Err(self),
        }
    }

    fn evaluation(&self) -> i32 {
        match self {
            TreeKind::Known(children) => children.first().unwrap().tree.evaluation,
            TreeKind::Unknown(speculation) => {
                let mut sum = 0;
                let mut n = 0;
                for children in speculation.iter().filter_map(|(_, c)| c.as_ref()) {
                    n += 1;
                    sum += children.first().map(|c| c.tree.evaluation).unwrap_or(-100);
                }
                sum / n
            }
        }
    }

    fn add_next_piece(&mut self, piece: Piece) {
        match self {
            TreeKind::Known(children) => {
                for child in children {
                    child.tree.add_next_piece(piece);
                }
            }
            TreeKind::Unknown(speculation) => {
                let mut now_known = vec![];
                std::mem::swap(speculation[piece].as_mut().unwrap(), &mut now_known);
                *self = TreeKind::Known(now_known);
            }
        }
    }

    fn expand(
        &mut self,
        mode: crate::moves::MovementMode,
        transient_weights: &crate::evaluation::BoardWeights,
        acc_weights: &crate::evaluation::PlacementWeights
    ) -> ExpandResult {
        let to_expand = match self {
            TreeKind::Known(children) => children,
            TreeKind::Unknown(speculation) => {
                let mut pieces = ArrayVec::<[Piece; 7]>::new();
                for (piece, children) in speculation.iter() {
                    if let Some(children) = children {
                        if !children.is_empty() {
                            pieces.push(piece);
                        }
                    }
                }
                speculation[*pieces.choose(&mut thread_rng()).unwrap()].as_mut().unwrap()
            }
        };
        if to_expand.is_empty() {
            return ExpandResult {
                depth: 0,
                new_nodes: 0,
                is_death: true
            }
        }

        let min = to_expand.last().unwrap().tree.evaluation;
        let weights = to_expand.iter()
            .enumerate()
            .map(|(i, c)| {
                let e = (c.tree.evaluation - min) as i64;
                e * e / (i + 1) as i64 + 1
            });
        let sampler = rand::distributions::WeightedIndex::new(weights).unwrap();
        let index = thread_rng().sample(sampler);

        let result = to_expand[index].tree.expand(mode, transient_weights, acc_weights);
        if result.is_death {
            to_expand.remove(index);
            match self {
                TreeKind::Known(children) => if children.is_empty() {
                    return ExpandResult {
                        is_death: true,
                        depth: result.depth + 1,
                        ..result
                    }
                }
                TreeKind::Unknown(speculation) => if speculation.iter()
                        .all(|(_, c)| c.as_ref().map(Vec::is_empty).unwrap_or(true)) {
                    return ExpandResult {
                        is_death: true,
                        depth: result.depth + 1,
                        ..result
                    }
                }
            }
            ExpandResult {
                is_death: false,
                depth: result.depth + 1,
                ..result
            }
        } else {
            to_expand.sort_by_key(|c| -c.tree.evaluation);
            ExpandResult {
                depth: result.depth + 1,
                ..result
            }
        }
    }
}