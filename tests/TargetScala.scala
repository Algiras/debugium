object TargetScala {
  def fibonacci(n: Int): List[Long] = {
    var fibs = List(0L, 1L)
    for (i <- 2 until n) {
      fibs = fibs :+ (fibs(i-1) + fibs(i-2))
    }
    fibs
  }

  def main(args: Array[String]): Unit = {
    println("TargetScala starting...")
    val count = 10
    val fibs = fibonacci(count)

    // breakpoint target ~ line 15
    val total = fibs.sum
    println(s"First $count Fibonacci numbers: $fibs")
    println(s"Sum: $total")

    fibs.zipWithIndex.foreach { case (f, i) =>
      println(s"  fib[$i] = $f")
    }
  }
}
