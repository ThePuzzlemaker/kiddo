use std::collections::BinaryHeap;
use std::cell::RefCell;

use num_traits::{Float, One, Zero};

use crate::heap_element::HeapElement;
use crate::util;

// TODO: get this working
/*
use std::alloc::{alloc_zeroed, dealloc, GlobalAlloc, System, Layout};
struct ZeroedAllocator;
unsafe impl GlobalAlloc for ZeroedAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        alloc_zeroed(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        dealloc(ptr, layout)
    }
}
#[global_allocator]
static GLOBAL: ZeroedAllocator = ZeroedAllocator;
*/

#[cfg_attr(feature = "serialize", derive(Serialize, Deserialize))]
#[derive(Clone, Debug)]
pub struct KdTree<A, T: std::cmp::PartialEq, U: AsRef<[A]>+ std::cmp::PartialEq> {
    // node
    left: Option<Box<KdTree<A, T, U>>>,
    right: Option<Box<KdTree<A, T, U>>>,
    // common
    dimensions: usize,
    capacity: usize,
    size: usize,
    min_bounds: Box<[A]>,
    max_bounds: Box<[A]>,
    // stem
    split_value: Option<A>,
    split_dimension: Option<usize>,
    // leaf
    points: Option<Vec<U>>,
    bucket: Option<Vec<T>>,
    
    #[serde(skip_deserializing)]
    // TODO: this is per-node. only really need per-tree
    distance_to_space_scratch_vec: Option<RefCell<Vec<A>>>,
}

#[derive(Debug, PartialEq)]
pub enum ErrorKind {
    WrongDimension,
    NonFiniteCoordinate,
    ZeroCapacity,
}

// TODO: use the zeroed allocator when allocating points. For points on a unit sphere, we can allocate
//       128bit aligned [f32; 3]'s (this should leave an empty 0 f32 in between each f32x3, assuming that
//       32 0's is an f32 0!. this can be cast to a __m128 packed f32x4 that we can then do a SIMD
//       dot product on instead of a squared euclidean. We would have to invert the distance comparison
//       as a higher valued DP between vecs on a unit sphere indicates smaller distance. This
//       should be significantly faster than sq_euclidean if the alignment can be done correctly and
//       the unused fourth element contains a 0f32.

impl<A: Float + Zero + One, T: std::cmp::PartialEq, U: AsRef<[A]> + std::cmp::PartialEq> KdTree<A, T, U> {
    pub fn new(dims: usize) -> Self {
        KdTree::with_capacity(dims, 2_usize.pow(4))
    }

    pub fn with_capacity(dimensions: usize, capacity: usize) -> Self {
        let min_bounds = vec![A::infinity(); dimensions];
        let max_bounds = vec![A::neg_infinity(); dimensions];
        KdTree {
            left: None,
            right: None,
            dimensions,
            capacity,
            size: 0,
            min_bounds: min_bounds.into_boxed_slice(),
            max_bounds: max_bounds.into_boxed_slice(),
            split_value: None,
            split_dimension: None,
            points: Some(Vec::with_capacity(capacity)),
            //points: Some(Vec::with_capacity_in(capacity, ZeroedAllocator)),
            //bucket: Some(vec![]),
            bucket: Some(Vec::with_capacity(capacity)),
            
            distance_to_space_scratch_vec: Some(RefCell::new(vec![A::nan(); dimensions])),
        }
    }

    pub fn size(&self) -> usize {
        self.size
    }
    
    pub fn allocate_dist_scratch(&mut self) {
        self.distance_to_space_scratch_vec = Some(RefCell::new(vec![A::nan(); self.dimensions]));
    }

    pub fn nearest<F>(
        &self,
        point: &[A],
        num: usize,
        distance: &F,
    ) -> Result<Vec<(A, &T)>, ErrorKind>
    where
        F: Fn(&[A], &[A]) -> A,
    {
        if let Err(err) = self.check_point(point) {
            return Err(err);
        }
        let num = std::cmp::min(num, self.size);
        if num == 0 {
            return Ok(vec![]);
        }
        let mut pending = BinaryHeap::new();
        let mut evaluated = BinaryHeap::<HeapElement<A, &T>>::new();
        pending.push(HeapElement {
            distance: A::zero(),
            element: self,
        });
        while !pending.is_empty()
            && (evaluated.len() < num
                || (-pending.peek().unwrap().distance <= evaluated.peek().unwrap().distance))
        {
            self.nearest_step(
                point,
                num,
                A::infinity(),
                distance,
                &mut pending,
                &mut evaluated,
            );
        }
        Ok(evaluated
            .into_sorted_vec()
            .into_iter()
            .take(num)
            .map(Into::into)
            .collect())
    }

    pub fn within<F>(&self, point: &[A], radius: A, distance: &F) -> Result<Vec<(A, &T)>, ErrorKind>
    where
        F: Fn(&[A], &[A]) -> A,
    {
        if let Err(err) = self.check_point(point) {
            return Err(err);
        }
        if self.size == 0 {
            return Ok(vec![]);
        }
        let mut pending = BinaryHeap::new();
        let mut evaluated = BinaryHeap::<HeapElement<A, &T>>::new();
        pending.push(HeapElement {
            distance: A::zero(),
            element: self,
        });
        while !pending.is_empty() && (-pending.peek().unwrap().distance <= radius) {
            self.nearest_step(
                point,
                self.size,
                radius,
                distance,
                &mut pending,
                &mut evaluated,
            );
        }
        Ok(evaluated
            .into_sorted_vec()
            //.into_vec()
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub fn best_n_within_into_iter<F>(&self, point: &[A], radius: A, max_qty: usize, distance: &F) -> impl Iterator<Item = T>
        where
            F: Fn(&[A], &[A]) -> A,
            T: Copy + Ord
    {
        // if let Err(err) = self.check_point(point) {
        //     return Err(err);
        // }
        // if self.size == 0 {
        //     return std::iter::empty::<T>();
        // }

        let mut pending = Vec::with_capacity(max_qty);
        let mut evaluated = BinaryHeap::<T>::new();

        pending.push(HeapElement {
            distance: A::zero(),
            element: self,
        });

        while !pending.is_empty() {
            self.best_n_within_step(
                point,
                self.size,
                max_qty,
                radius,
                distance,
                &mut pending,
                &mut evaluated,
            );
        }

        evaluated.into_iter()
    }

    pub fn best_n_within<F>(&self, point: &[A], radius: A, max_qty: usize, distance: &F) -> Result<Vec<T>, ErrorKind>
        where
            F: Fn(&[A], &[A]) -> A,
            T: Copy + Ord
    {
        if let Err(err) = self.check_point(point) {
            return Err(err);
        }
        if self.size == 0 {
            return Ok(vec![]);
        }

        let mut pending = Vec::with_capacity(max_qty);
        let mut evaluated = BinaryHeap::<T>::new();

        pending.push(HeapElement {
            distance: A::zero(),
            element: self,
        });

        while !pending.is_empty() {
            self.best_n_within_step(
                point,
                self.size,
                max_qty,
                radius,
                distance,
                &mut pending,
                &mut evaluated,
            );
        }

        Ok(evaluated
            .into_vec()
            .into_iter()
            .collect())
    }

    fn best_n_within_step<'b, F>(
        &self,
        point: &[A],
        num: usize,
        max_qty: usize,
        max_dist: A,
        distance: &F,
        pending: &mut Vec<HeapElement<A, &'b Self>>,
        evaluated: &mut BinaryHeap<T>,
    ) where
        F: Fn(&[A], &[A]) -> A,
        T: Copy + Ord
    {
        let mut curr = &*pending.pop().unwrap().element;
        debug_assert!(evaluated.len() <= num);
        
        // TODO: ensure that self.distance_to_space_scratch_vec gets initialised by serde so it does not need to be optional
        let mut scratch_vec = self.distance_to_space_scratch_vec.as_ref().unwrap().borrow_mut();

        while !curr.is_leaf() {
            let candidate;
            if curr.belongs_in_left(point) {
                candidate = curr.right.as_ref().unwrap();
                curr = curr.left.as_ref().unwrap();
            } else {
                candidate = curr.left.as_ref().unwrap();
                curr = curr.right.as_ref().unwrap();
            }
            
            let candidate_to_space = util::distance_to_space_noalloc(
                point,
                &*candidate.min_bounds,
                &*candidate.max_bounds,
                distance,
                self.dimensions,
                &mut scratch_vec,
            );
            if candidate_to_space <= max_dist {
                pending.push(HeapElement {
                    distance: candidate_to_space * -A::one(),
                    element: &**candidate,
                });
            }
        }

        let points = curr.points.as_ref().unwrap().iter();
        let bucket = curr.bucket.as_ref().unwrap().iter();
        let iter = points.zip(bucket).map(|(p, d)| HeapElement {
            distance: distance(point, p.as_ref()),
            element: d,
        });

        for element in iter {
            if element <= max_dist {
                if evaluated.len() < max_qty {
                    evaluated.push(*element.element);
                } else {
                    // evaluated.pop();
                    // evaluated.push(*element.element);
                    //
                    let mut top = evaluated.peek_mut().unwrap();
                    if element.element < &top {
                        *top = *element.element;
                    }
                }
            }
        }
    }

    fn nearest_step<'b, F>(
        &self,
        point: &[A],
        num: usize,
        max_dist: A,
        distance: &F,
        pending: &mut BinaryHeap<HeapElement<A, &'b Self>>,
        evaluated: &mut BinaryHeap<HeapElement<A, &'b T>>,
    ) where
        F: Fn(&[A], &[A]) -> A,
    {
        let mut curr = &*pending.pop().unwrap().element;
        debug_assert!(evaluated.len() <= num);
        let evaluated_dist = if evaluated.len() == num {
            // We only care about the nearest `num` points, so if we already have `num` points,
            // any more point we add to `evaluated` must be nearer then one of the point already in
            // `evaluated`.
            max_dist.min(evaluated.peek().unwrap().distance)
        } else {
            max_dist
        };

        while !curr.is_leaf() {
            let candidate;
            if curr.belongs_in_left(point) {
                candidate = curr.right.as_ref().unwrap();
                curr = curr.left.as_ref().unwrap();
            } else {
                candidate = curr.left.as_ref().unwrap();
                curr = curr.right.as_ref().unwrap();
            }
            let candidate_to_space = util::distance_to_space(
                point,
                &*candidate.min_bounds,
                &*candidate.max_bounds,
                distance,
                self.dimensions
            );
            if candidate_to_space <= evaluated_dist {
                pending.push(HeapElement {
                    distance: candidate_to_space * -A::one(),
                    element: &**candidate,
                });
            }
        }

        let points = curr.points.as_ref().unwrap().iter();
        let bucket = curr.bucket.as_ref().unwrap().iter();
        let iter = points.zip(bucket).map(|(p, d)| HeapElement {
            distance: distance(point, p.as_ref()),
            element: d,
        });
        for element in iter {
            if element <= max_dist {
                if evaluated.len() < num {
                    evaluated.push(element);
                // } else if element < *evaluated.peek().unwrap() {
                //     evaluated.pop();
                //     evaluated.push(element);
                } else {
                    let mut top = evaluated.peek_mut().unwrap();
                    if element < *top {
                        *top = element;
                    }
                }
            }
        }
    }

    pub fn iter_nearest<'a, 'b, F>(
        &'b self,
        point: &'a [A],
        distance: &'a F,
    ) -> Result<NearestIter<'a, 'b, A, T, U, F>, ErrorKind>
    where
        F: Fn(&[A], &[A]) -> A,
    {
        if let Err(err) = self.check_point(point) {
            return Err(err);
        }
        let mut pending = BinaryHeap::new();
        let evaluated = BinaryHeap::<HeapElement<A, &T>>::new();
        pending.push(HeapElement {
            distance: A::zero(),
            element: self,
        });
        Ok(NearestIter {
            point,
            pending,
            evaluated,
            distance,
            dimensions: self.dimensions
        })
    }

    pub fn iter_nearest_mut<'a, 'b, F>(
        &'b mut self,
        point: &'a [A],
        distance: &'a F,
    ) -> Result<NearestIterMut<'a, 'b, A, T, U, F>, ErrorKind>
    where
        F: Fn(&[A], &[A]) -> A,
    {
        if let Err(err) = self.check_point(point) {
            return Err(err);
        }
        let mut pending = BinaryHeap::new();
        let evaluated = BinaryHeap::<HeapElement<A, &mut T>>::new();

        let dimensions = self.dimensions;

        pending.push(HeapElement {
            distance: A::zero(),
            element: self,
        });
        Ok(NearestIterMut {
            point,
            pending,
            evaluated,
            distance,
            dimensions,
        })
    }

    pub fn add(&mut self, point: U, data: T) -> Result<(), ErrorKind> {
        if self.capacity == 0 {
            return Err(ErrorKind::ZeroCapacity);
        }
        if let Err(err) = self.check_point(point.as_ref()) {
            return Err(err);
        }
        self.add_unchecked(point, data)
    }

    fn add_unchecked(&mut self, point: U, data: T) -> Result<(), ErrorKind> {
        if self.is_leaf() {
            self.add_to_bucket(point, data);
            return Ok(());
        }
        self.extend(point.as_ref());
        self.size += 1;
        let next = if self.belongs_in_left(point.as_ref()) {
            self.left.as_mut()
        } else {
            self.right.as_mut()
        };
        next.unwrap().add_unchecked(point, data)
    }

    fn add_to_bucket(&mut self, point: U, data: T) {
        self.extend(point.as_ref());
        let mut points = self.points.take().unwrap();
        let mut bucket = self.bucket.take().unwrap();
        points.push(point);
        bucket.push(data);
        self.size += 1;
        if self.size > self.capacity {
            self.split(points, bucket);
        } else {
            self.points = Some(points);
            self.bucket = Some(bucket);
        }
    }

    pub fn remove(&mut self, point: &U, data: &T) -> Result<usize, ErrorKind> {
        let mut removed = 0;
        if let Err(err) = self.check_point(point.as_ref()) {
            return Err(err);
        }
        if let (Some(mut points), Some(mut bucket)) = (self.points.take(), self.bucket.take()) {
            while let Some(p_index) = points.iter().position(|x| x == point) {
                if &bucket[p_index] == data {
                    points.remove(p_index);
                    bucket.remove(p_index);
                    removed += 1;
                    self.size -= 1;
                }
            }
            self.points = Some(points);
            self.bucket = Some(bucket);
        } else {
            if let Some(right) = self.right.as_mut() {
                let right_removed = right.remove(point, data)?;
                if right_removed > 0 {
                    self.size -= right_removed;
                    removed += right_removed;
                }
            }
            if let Some(left) = self.left.as_mut() {
                let left_removed = left.remove(point, data)?;
                if left_removed > 0 {
                    self.size -= left_removed;
                    removed += left_removed;
                }
            }
        }
        Ok(removed)
    }

    fn split(&mut self, mut points: Vec<U>, mut bucket: Vec<T>) {
        let mut max = A::zero();
        for dim in 0..self.dimensions {
            let diff = self.max_bounds[dim] - self.min_bounds[dim];
            if !diff.is_nan() && diff > max {
                max = diff;
                self.split_dimension = Some(dim);
            }
        }
        match self.split_dimension {
            None => {
                self.points = Some(points);
                self.bucket = Some(bucket);
                return;
            }
            Some(dim) => {
                let min = self.min_bounds[dim];
                let max = self.max_bounds[dim];
                self.split_value = Some(min + (max - min) / A::from(2.0).unwrap());
            }
        };
        let mut left = Box::new(KdTree::with_capacity(self.dimensions, self.capacity));
        let mut right = Box::new(KdTree::with_capacity(self.dimensions, self.capacity));
        while !points.is_empty() {
            let point = points.swap_remove(0);
            let data = bucket.swap_remove(0);
            if self.belongs_in_left(point.as_ref()) {
                left.add_to_bucket(point, data);
            } else {
                right.add_to_bucket(point, data);
            }
        }
        self.left = Some(left);
        self.right = Some(right);
    }

    fn belongs_in_left(&self, point: &[A]) -> bool {
        point[self.split_dimension.unwrap()] < self.split_value.unwrap()
    }

    fn extend(&mut self, point: &[A]) {
        let min = self.min_bounds.iter_mut();
        let max = self.max_bounds.iter_mut();
        for ((l, h), v) in min.zip(max).zip(point.iter()) {
            if v < l {
                *l = *v
            }
            if v > h {
                *h = *v
            }
        }
    }

    fn is_leaf(&self) -> bool {
        self.split_dimension.is_none()
    }

    fn check_point(&self, point: &[A]) -> Result<(), ErrorKind> {
        //if self.dimensions != point.len() {
        if self.dimensions > point.len() {
            return Err(ErrorKind::WrongDimension);
        }
        for n in point {
            if !n.is_finite() {
                return Err(ErrorKind::NonFiniteCoordinate);
            }
        }
        Ok(())
    }
}

pub struct NearestIter<
    'a,
    'b,
    A: 'a + 'b + Float,
    T: 'b + PartialEq,
    U: 'b + AsRef<[A]> + std::cmp::PartialEq,
    F: 'a + Fn(&[A], &[A]) -> A,
> {
    point: &'a [A],
    pending: BinaryHeap<HeapElement<A, &'b KdTree<A, T, U>>>,
    evaluated: BinaryHeap<HeapElement<A, &'b T>>,
    distance: &'a F,
    dimensions: usize
}

impl<'a, 'b, A: Float + Zero + One, T: 'b, U: 'b + AsRef<[A]>, F: 'a> Iterator
    for NearestIter<'a, 'b, A, T, U, F>
where
    F: Fn(&[A], &[A]) -> A, U: PartialEq, T: PartialEq
{
    type Item = (A, &'b T);
    fn next(&mut self) -> Option<(A, &'b T)> {
        use util::distance_to_space;

        let distance = self.distance;
        let point = self.point;
        while !self.pending.is_empty()
            && (self.evaluated.peek().map_or(A::infinity(), |x| -x.distance)
                >= -self.pending.peek().unwrap().distance)
        {
            let mut curr = &*self.pending.pop().unwrap().element;
            while !curr.is_leaf() {
                let candidate;
                if curr.belongs_in_left(point) {
                    candidate = curr.right.as_ref().unwrap();
                    curr = curr.left.as_ref().unwrap();
                } else {
                    candidate = curr.left.as_ref().unwrap();
                    curr = curr.right.as_ref().unwrap();
                }
                self.pending.push(HeapElement {
                    distance: -distance_to_space(
                        point,
                        &*candidate.min_bounds,
                        &*candidate.max_bounds,
                        distance,
                        self.dimensions
                    ),
                    element: &**candidate,
                });
            }
            let points = curr.points.as_ref().unwrap().iter();
            let bucket = curr.bucket.as_ref().unwrap().iter();
            self.evaluated
                .extend(points.zip(bucket).map(|(p, d)| HeapElement {
                    distance: -distance(point, p.as_ref()),
                    element: d,
                }));
        }
        self.evaluated.pop().map(|x| (-x.distance, x.element))
    }
}

pub struct NearestIterMut<
    'a,
    'b,
    A: 'a + 'b + Float,
    T: 'b + PartialEq,
    U: 'b + AsRef<[A]> + PartialEq,
    F: 'a + Fn(&[A], &[A]) -> A,
> {
    point: &'a [A],
    pending: BinaryHeap<HeapElement<A, &'b mut KdTree<A, T, U>>>,
    evaluated: BinaryHeap<HeapElement<A, &'b mut T>>,
    distance: &'a F,
    dimensions: usize
}

impl<'a, 'b, A: Float + Zero + One, T: 'b, U: 'b + AsRef<[A]>, F: 'a> Iterator
    for NearestIterMut<'a, 'b, A, T, U, F>
where
    F: Fn(&[A], &[A]) -> A, U: PartialEq, T: PartialEq
{
    type Item = (A, &'b mut T);
    fn next(&mut self) -> Option<(A, &'b mut T)> {
        use util::distance_to_space;

        let distance = self.distance;
        let point = self.point;
        while !self.pending.is_empty()
            && (self.evaluated.peek().map_or(A::infinity(), |x| -x.distance)
                >= -self.pending.peek().unwrap().distance)
        {
            let mut curr = &mut *self.pending.pop().unwrap().element;
            while !curr.is_leaf() {
                let candidate;
                if curr.belongs_in_left(point) {
                    candidate = curr.right.as_mut().unwrap();
                    curr = curr.left.as_mut().unwrap();
                } else {
                    candidate = curr.left.as_mut().unwrap();
                    curr = curr.right.as_mut().unwrap();
                }
                self.pending.push(HeapElement {
                    distance: -distance_to_space(
                        point,
                        &*candidate.min_bounds,
                        &*candidate.max_bounds,
                        distance,
                        self.dimensions
                    ),
                    element: &mut **candidate,
                });
            }
            let points = curr.points.as_ref().unwrap().iter();
            let bucket = curr.bucket.as_mut().unwrap().iter_mut();
            self.evaluated
                .extend(points.zip(bucket).map(|(p, d)| HeapElement {
                    distance: -distance(point, p.as_ref()),
                    element: d,
                }));
        }
        self.evaluated.pop().map(|x| (-x.distance, x.element))
    }
}

impl std::error::Error for ErrorKind {
    fn description(&self) -> &str {
        match *self {
            ErrorKind::WrongDimension => "wrong dimension",
            ErrorKind::NonFiniteCoordinate => "non-finite coordinate",
            ErrorKind::ZeroCapacity => "zero capacity",
        }
    }
}

impl std::fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "KdTree error: {}", self)
    }
}

#[cfg(test)]
mod tests {
    extern crate rand;
    use super::KdTree;

    fn random_point() -> ([f64; 2], i32) {
        rand::random::<([f64; 2], i32)>()
    }

    #[test]
    fn it_has_default_capacity() {
        let tree: KdTree<f64, i32, [f64; 2]> = KdTree::new(2);
        assert_eq!(tree.capacity, 2_usize.pow(4));
    }

    #[test]
    fn it_can_be_cloned() {
        let mut tree: KdTree<f64, i32, [f64; 2]> = KdTree::new(2);
        let (pos, data) = random_point();
        tree.add(pos, data).unwrap();
        let mut cloned_tree = tree.clone();
        cloned_tree.add(pos, data).unwrap();
        assert_eq!(tree.size(), 1);
        assert_eq!(cloned_tree.size(), 2);
    }

    #[test]
    fn it_holds_on_to_its_capacity_before_splitting() {
        let mut tree: KdTree<f64, i32, [f64; 2]> = KdTree::new(2);
        let capacity = 2_usize.pow(4);
        for _ in 0..capacity {
            let (pos, data) = random_point();
            tree.add(pos, data).unwrap();
        }
        assert_eq!(tree.size, capacity);
        assert_eq!(tree.size(), capacity);
        assert!(tree.left.is_none() && tree.right.is_none());
        {
            let (pos, data) = random_point();
            tree.add(pos, data).unwrap();
        }
        assert_eq!(tree.size, capacity + 1);
        assert_eq!(tree.size(), capacity + 1);
        assert!(tree.left.is_some() && tree.right.is_some());
    }

    #[test]
    fn no_items_can_be_added_to_a_zero_capacity_kdtree() {
        let mut tree: KdTree<f64, i32, [f64; 2]> = KdTree::with_capacity(2, 0);
        let (pos, data) = random_point();
        let res = tree.add(pos, data);
        assert!(res.is_err());
    }
}
